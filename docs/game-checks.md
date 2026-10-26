# Game checks

`godot_game_scenario` plays one input sequence in a running game and returns the snapshots, warnings and errors from that call. It uses the existing held stepper (`godot_game_physics`), input injection (`godot_game_input`) and, when asked, one screenshot (`godot_game_screenshot`). The caller does not write a GDScript file for the scenario.

## Start

```json
{"mode": "process", "held": true, "scene": "res://scenes/main.tscn"}
```

`held: true` fixes the frame rate to the project's physics tick rate, so each tick is paired with one frame. Without it the scenario still runs, and the answer says `"deterministic": false` with `deterministic_reason` `not_started_held`.

A scenario selects one managed process at entry and keeps that target for input, snapshots, logs and cleanup, even if another process starts. Start editor-owned games with `mode: "process"` to use this tool.

The call leaves the game held. Nothing in the game advances until `godot_game_physics` `{"action": "resume"}` or `godot_stop_project`.

## One step

`frames` is how many physics ticks run after the step's input is applied. `0` applies the input and reads state without stepping; a tap is refused there, because no tick would see it down. Each step is at most 600 ticks, and the whole call at most 3600.

`actions` names an Input Map action:

| `phase` | What the game sees |
| --- | --- |
| `tap` (default) | Down for this step's ticks, at `strength` (0 to 1, default 1), then released before the next step. Needs `frames` of at least 1. |
| `press` | Down for this step and still down afterwards, until a later `release`. |
| `release` | Let go at the start of the step. |

The default is the same as `godot_game_input`. Pointers use the same phases and default, plus `move`.

`pointers` is mouse or touch. `x` and `y` are in the call's `coordinate_space`: `viewport` (default) is pixels of the root viewport's visible rectangle, the same space `_input` sees; `window` is pixels of the game window. In a 1920 x 1080 project shown in a 1280 x 720 window, window (640, 360) is viewport (960, 540). The answer repeats `coordinate_space`. Several touches are separate `index` values and may be down together. `phase` `move` drags a touch that is already down (or moves the mouse). A touch also emits the engine's emulated mouse event when that project setting is on.

`pause` sets `SceneTree.paused` before the ticks. Pausable nodes skip `_physics_process`. The runtime still answers: its node ignores pause, and a hold serves requests from the physics tick, which the tree still emits while paused. The game's own pause does not release the hold.

`bodies` lists CharacterBody2D or CharacterBody3D paths. Each snapshot has `position`, `velocity`, `is_on_floor`, `is_on_wall`, `is_on_ceiling`, `slide_count` and up to 8 `collisions` (`normal`, `position`, `collider` path). Another class, or a missing node, is an error that names the path. `nodes` reads public properties through `game.get` (no method calls, no private names).

Limits: at most 32 steps, 32 actions and pointers in one step, 16 `bodies`, 16 `nodes` with at most 32 properties each, names of at most 256 characters, and touch `index` 0 to 9. A property name whose `/` or `:` path has a segment starting with `_` is refused. Only one scenario plays in a run at a time; a second call is refused until the first has released its input.

`screenshot` true captures one frame of what the game last drew. A headless game has no image; that is a warning on the scenario, not a fake picture. The image is the same PNG `godot_game_screenshot` returns.

## Answer

`steps` is one object per step that ran, with `ticks_run` and `snapshot`. `ticks_run` at the top is the sum. `deterministic` and `deterministic_reason` come from the held stepper (`not_started_held`, `fixed_rate_differs`, `frames_did_not_follow_ticks`). `output.lines` and `output.errors` are warnings and errors whose sequence number is at least the cursor taken at the start of this call. Older lines from the same process are not included, and another run has its own log.

`ok` is false when a step fails, when a body or node cannot be read, or when cleanup fails. A game error that only shows up in `output.errors` does not change `ok`; read it there.

`released` is the `input.release_all` answer. `held` inside it is what is still down afterwards, which is empty unless the release itself failed. A failed step, a timeout and a cancelled call release the same way. Stopping the game releases input from the runtime's exit.

The whole call has one 60-second deadline. The final 3 seconds are reserved for input cleanup; setup, input, physics, snapshots and the screenshot share the preceding budget. Cleanup remains armed until its acknowledgement, so cancelling during cleanup retries release on the original process. If cleanup fails, the report is an error and includes `released.error`. A timed-out physics step may still finish before the game holds again.

## What is exact, and what is not

Exact means the number of physics ticks. A press is flushed before those ticks, so `_input` sees the event immediately and `_process` sees the action on the following frame. `is_action_just_pressed` is therefore true on an early tick of the step, not on a tick of your choosing inside it.

Not exact: threads, file loading, GPU work, and the physics solver's contact details. Two runs with the same scenario and the same scene agree on the tick count. They are not a promise that every float in a contact manifold matches.

## Example

```json
{
  "steps": [
    {"frames": 10},
    {"frames": 20, "actions": [
      {"action": "move_right", "phase": "press", "strength": 1},
      {"action": "jump", "phase": "tap"}
    ]},
    {"frames": 45}
  ],
  "bodies": ["Player"],
  "nodes": [{"path": "Player", "properties": ["jump_count"]}]
}
```

The first step lets the body settle. The second holds move and taps jump for 20 ticks. The third continues with move still held (a press stays down) and jump already released.

## Run the checks

```bash
cargo test --offline --test tools_test a_scenario_is_refused_before_a_game_exists
GODOT_PATH=/path/to/godot cargo test --test scenario_e2e_test -- --ignored --test-threads=1
```

`scenario_e2e_test` needs the Godot build named in `.github/godot-version`.

## Backlog

Not in this stage:

- Mobile viewport profiles and safe area. The game already reads `--mobile-preview=` and `--safe-preview=`; the scenario tool does not set a device profile or assert the safe rect.
- Copying Godot resources between projects with their dependencies and UIDs.
- A batch of editor edits with one undo that rolls the whole batch back.
