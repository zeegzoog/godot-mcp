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
use crate::util::lock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

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
    /// The project as it was before the first of them started.
    before: Option<Arc<TextResourceSnapshot>>,
}

/// A script run's place in its project's batch. Dropping it ends the run's part in the batch.
pub(super) struct ScriptTurn {
    gate: Arc<Gate>,
    snapshot: Arc<TextResourceSnapshot>,
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
        let snapshot = {
            let mut batch = lock(&gate.batch);
            batch.runs += 1;
            batch.joined += 1;
            match &batch.before {
                Some(snapshot) => snapshot.clone(),
                None => {
                    let snapshot = Arc::new(TextResourceSnapshot::take(project));
                    batch.before = Some(snapshot.clone());
                    snapshot
                }
            }
        };
        (ScriptTurn { gate, snapshot }, running)
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
    pub(super) fn snapshot(&self) -> &TextResourceSnapshot {
        &self.snapshot
    }

    /// Whether another script run in the project worked at the same time as this one.
    pub(super) fn overlapped(&self) -> bool {
        lock(&self.gate.batch).joined > 1
    }

    pub(super) async fn exclusive(&self) -> OwnedRwLockWriteGuard<()> {
        self.gate.order.clone().write_owned().await
    }
}

impl Drop for ScriptTurn {
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
