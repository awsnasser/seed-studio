# Canvas and execution

The canvas uses React Flow for editing and a Rust core for validation, persistence and generation. Each project has its own graph and media. The chat and canvas share provider clients and saved assets.

## Nodes and connections

Prompt and text nodes produce text. Imported images and Seedream nodes produce image references. Seedance nodes accept prompts and model-compatible frame, image and audio inputs. Notes and groups organize the board.

The Rust validator checks node types, ports, project membership and cycles. A graph can remain incomplete while being edited. A run plan checks the selected nodes and their dependencies before a request starts.

## Changes and history

Manual changes and assistant-generated patches pass through the same graph validator. Patches are applied atomically with their undo history. The assistant can inspect the canvas, discover model options, propose edits and plan a run. Generation follows the application's run controls.

## Requests and saved results

The execution engine fingerprints the prompt, model, region, options and inputs. It reuses a completed result when inputs match and starts a new request when the user explicitly regenerates or changes those inputs. Moving a node or changing the theme does not invalidate a generation.

A claimed run prevents duplicate submission. The app records a provider task ID as soon as it is received. Video tasks can resume polling after restart. A request with an unknown outcome is presented for the user to resolve rather than automatically submitted again.

Single-node and batch generation show the planned request count. Stop the rest prevents queued work from starting. Provider requests already accepted may still complete and incur charges.

## Source map

| File | Responsibility |
| --- | --- |
| `src/components/Canvas.tsx` | Canvas editing and node panels |
| `src-tauri/src/graph.rs` | Graph types, ports and patch validation |
| `src-tauri/src/engine.rs` | Plans, dependencies, fingerprints and execution |
| `src-tauri/src/agent.rs` | Canvas assistant tools |
| `src-tauri/src/video.rs` | Video task validation, submission and polling |
| `src-tauri/src/core.rs` | Project state, storage and undo history |

New installations start without projects, nodes or media. Create a named project and add your own content. The integration tests build their own synthetic graph and reference pattern; these are not bundled with the app.
