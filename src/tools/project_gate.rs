//! Orders the headless Godot processes that share one project directory.
//!
//! Script runs only read the project and write their own files, so any number of them may run
//! at once. What reads or rewrites files other processes may be writing waits for them instead:
//! the resource repair after a script run (it rereads every scene and resource the run may have
//! touched and records uids in `.godot/uid_cache.bin`), and `godot --import` before an export or
//! for `godot_resave_resources` (it rewrites `.godot/imported` and the uid cache). Different
//! projects never wait for each other.
//!
//! No guard here is held across an editor round-trip: callers ask the editor before they take a
//! guard or after they drop it.

use crate::godot::TextResourceSnapshot;
use crate::util::{blocking, lock};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{OnceCell, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

#[derive(Default)]
pub(super) struct ProjectGates(Mutex<HashMap<PathBuf, Arc<Gate>>>);

#[derive(Default)]
struct Gate {
    /// Shared by the running processes, exclusive for a repair or an import. Tokio's lock is
    /// fair: a waiting exclusive holder keeps new processes from starting before it.
    order: Arc<RwLock<()>>,
    batch: Mutex<Batch>,
}

/// Script runs in a project whose times overlap, from the first one's start to the last one's
/// repair.
#[derive(Default)]
struct Batch {
    runs: usize,
    /// Every run that joined the batch, finished or not.
    joined: usize,
    /// The project as it was before the first of them started. The first run takes it off the
    /// async workers and without holding the batch's lock; a run that joins meanwhile waits for
    /// that snapshot instead of taking its own.
    before: Arc<OnceCell<Arc<TextResourceSnapshot>>>,
}

/// A script run's place in its project's batch. Dropping it ends the run's part in the batch.
pub(super) struct ScriptTurn {
    member: BatchMember,
    snapshot: Arc<TextResourceSnapshot>,
}

/// A run counted in its project's batch; dropping it, even while the run still waits for the
/// batch's snapshot, takes the run out again.
struct BatchMember {
    gate: Arc<Gate>,
}

impl ProjectGates {
    fn gate(&self, project: &Path) -> Arc<Gate> {
        let key = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        lock(&self.0).entry(key).or_default().clone()
    }

    /// Waits until no repair or import runs in the project, then joins its batch of script
    /// runs. The returned guard is held while the run's Godot process works. The turn's snapshot
    /// is the project before the batch's first run, so a run that starts while another one is
    /// writing never takes that run's half-written files for the state before it.
    pub(super) async fn begin_script(
        &self,
        project: &Path,
    ) -> (ScriptTurn, OwnedRwLockReadGuard<()>) {
        let gate = self.gate(project);
        let running = gate.order.clone().read_owned().await;
        let before = {
            let mut batch = lock(&gate.batch);
            batch.runs += 1;
            batch.joined += 1;
            batch.before.clone()
        };
        let member = BatchMember { gate };
        let snapshot = before
            .get_or_init(|| {
                let project = project.to_path_buf();
                async move {
                    Arc::new(blocking(move || TextResourceSnapshot::take(&project)).await)
                }
            })
            .await
            .clone();
        (ScriptTurn { member, snapshot }, running)
    }

    /// Waits for an import or export running in the project to finish, so a game started now
    /// loads the files it left; holds nothing afterwards, since a game may run for a long time
    /// without writing the project.
    pub(super) async fn settle(&self, project: &Path) {
        drop(self.gate(project).order.clone().read_owned().await);
    }

    /// Held while nothing else may run Godot in the project: it waits for the running processes
    /// and keeps new ones from starting.
    pub(super) async fn exclusive(&self, project: &Path) -> OwnedRwLockWriteGuard<()> {
        self.gate(project).order.clone().write_owned().await
    }
}

impl ScriptTurn {
    pub(super) fn snapshot(&self) -> &Arc<TextResourceSnapshot> {
        &self.snapshot
    }

    /// Whether another script run in the project worked at the same time as this one.
    pub(super) fn overlapped(&self) -> bool {
        lock(&self.member.gate.batch).joined > 1
    }

    pub(super) async fn exclusive(&self) -> OwnedRwLockWriteGuard<()> {
        self.member.gate.order.clone().write_owned().await
    }
}

impl Drop for BatchMember {
    fn drop(&mut self) {
        let mut batch = lock(&self.gate.batch);
        batch.runs = batch.runs.saturating_sub(1);
        if batch.runs == 0 {
            *batch = Batch::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(dir.path().join("project.godot"), "").expect("project.godot");
        dir
    }

    #[tokio::test]
    async fn runs_that_overlap_share_the_snapshot_of_the_first_one() {
        let dir = project();
        let gates = ProjectGates::default();
        let (first, _running) = gates.begin_script(dir.path()).await;
        assert!(!first.overlapped());
        let first_snapshot = first.snapshot.clone();
        std::fs::write(dir.path().join("half.tscn"), "[gd_scene format=3]\n").expect("write");
        let (second, _also_running) = gates.begin_script(dir.path()).await;
        assert!(Arc::ptr_eq(&first.snapshot, &second.snapshot));
        assert!(first.overlapped() && second.overlapped());
        drop(first);
        assert!(
            second.overlapped(),
            "it overlapped even though the other one ended"
        );
        drop(second);
        let (third, _later) = gates.begin_script(dir.path()).await;
        assert!(!third.overlapped());
        assert!(
            !Arc::ptr_eq(&first_snapshot, &third.snapshot),
            "a new batch takes a new snapshot"
        );
    }

    #[tokio::test]
    async fn a_run_dropped_while_the_snapshot_is_taken_leaves_the_batch() {
        let dir = project();
        // Enough files that the snapshot is still being taken after the first poll.
        for index in 0..400 {
            std::fs::write(
                dir.path().join(format!("s{index}.tscn")),
                "[gd_scene format=3 uid=\"uid://a\"]\n",
            )
            .expect("scene");
        }
        let gates = ProjectGates::default();
        tokio::select! {
            biased;
            _ = gates.begin_script(dir.path()) => {}
            () = std::future::ready(()) => {}
        }
        let (turn, _running) = gates.begin_script(dir.path()).await;
        assert!(!turn.overlapped(), "the dropped run left the batch");
        drop(turn);
        let gate = gates.gate(dir.path());
        assert_eq!(lock(&gate.batch).runs, 0);
    }

    #[tokio::test]
    async fn the_snapshot_is_taken_without_holding_the_batch_lock() {
        let dir = project();
        for index in 0..400 {
            std::fs::write(
                dir.path().join(format!("s{index}.tscn")),
                "[gd_scene format=3]\n",
            )
            .expect("scene");
        }
        let gates = Arc::new(ProjectGates::default());
        let first = tokio::spawn({
            let (gates, path) = (gates.clone(), dir.path().to_path_buf());
            async move { gates.begin_script(&path).await.0 }
        });
        // The first run starts its snapshot and gives the (only) worker back while it is taken:
        // the batch can be read meanwhile and a second run joins it.
        tokio::task::yield_now().await;
        assert!(
            !first.is_finished(),
            "the snapshot is taken off the async workers"
        );
        let gate = gates.gate(dir.path());
        assert!(gate.batch.try_lock().is_ok(), "the batch lock is free");
        let (second, _running) = gates.begin_script(dir.path()).await;
        let first = first.await.expect("first run");
        assert!(Arc::ptr_eq(&first.snapshot, &second.snapshot));
        assert!(first.overlapped() && second.overlapped());
    }

    #[tokio::test]
    async fn exclusive_waits_for_shared_and_other_projects_do_not() {
        let (one, two) = (project(), project());
        let gates = Arc::new(ProjectGates::default());
        let (_turn, running) = gates.begin_script(one.path()).await;
        let other_project =
            tokio::time::timeout(Duration::from_millis(200), gates.exclusive(two.path())).await;
        assert!(other_project.is_ok(), "another project must not wait");
        drop(other_project);
        let waiting =
            tokio::time::timeout(Duration::from_millis(200), gates.exclusive(one.path())).await;
        assert!(
            waiting.is_err(),
            "a repair must wait for the running process"
        );
        drop(running);
        let after =
            tokio::time::timeout(Duration::from_millis(200), gates.exclusive(one.path())).await;
        assert!(after.is_ok());
    }
}
