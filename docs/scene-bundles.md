# Experimental scene bundles

The native scene viewer opens an original **version-1 scene bundle**, with a
`scene.json` manifest and six fixed local members. It does not open arbitrary
source formats directly. It is an experimental example client, separate from
the main desktop workbench.

```sh
cargo run -p craft-anything-ui-egui --example scene_viewer -- /absolute/bundle/scene.json
```

Drag to orbit, Shift-drag or right-drag to pan, and scroll to zoom. Click a surface
to select its mesh instance. The resource tree selects a resource; Hide and Isolate
include its descendant resources. Fit selection frames the selected instance or
the geometry of the selected resource and its descendants. Show all resets filters.
Choosing Y up changes the camera only. Source units and axes remain unverified.
The viewer uses two-sided placeholder shading; source materials, texture assets,
visibility rules and visual parity are not established. It does not evaluate
schedules, time-based appearance, costs or engineering quantities.

All file loading and surface selection run through engine tools on workers:

| Tool | Result |
|---|---|
| `formats_scene_bundle_open` | Validates and retains a bundle; returns its content fingerprint, declared provenance, counts and bounds. |
| `formats_scene_resources_list` | Pages resource GUIDs, names, parent GUIDs and directly associated instance counts. |
| `formats_scene_surface_pick` | Tests a world-space ray against visible triangles; returns the nearest hit, or explicitly incomplete selection. |

These tools require Deep mode. They perform no source execution and open no
dialogs. CLI example: `craft-anything-cli --deep formats-scene-bundle-open /absolute/bundle/scene.json`.
Resources and picking use the same retained session as Open (for example, a
long-running MCP session); independent CLI invocations do not share that cache.
Pick requires the current `scene_id` and accepts resource GUID and instance ID
filters. A stale scene or unknown filter fails instead of selecting another model.
`source_sha256_declared` is creator-supplied provenance, not independent proof of
the source file. `scene_id` fingerprints the seven members actually consumed.
Follow-up evidence preserves the manifest's real subject identity.

The [version-1 schema](schemas/scene-bundle-v1.json) documents the manifest,
JSONL records and packed geometry. The reader validates fields it consumes; it does
not perform full JSON Schema validation of ignored exporter diagnostics. Positions are little-endian f64 XYZ triples;
triangle indices are little-endian mesh-local u32 triples. The reader also checks
counts, dense byte ranges, indices, finite affine transforms, exact local bounds,
graph consistency and cycles, rooted instance paths, composed matrices,
deterministic instance IDs and resource membership. Metadata project schema 4
supplies resource identities; other project fields are not interpreted.

Unknown versions and encodings are rejected. There are no older bundle versions.
A future breaking change requires a new schema and explicit offline migration
to a new folder, preserving the original bundle. Version 1 remains partial and
does not imply compatibility with an untested source application or file.

Input is capped at 384 MiB total, 256 MiB geometry, 64 MiB per metadata member,
100,000 nodes/instances, ten million vertices and eight million definition
triangles. Resource hierarchies and instance paths are depth-bounded. Selection
tests at most two million triangles; cancellation, a work-budget exhaustion or
a relevant singular transform produces no certain hit. GPU buffers are checked
against adapter limits; offscreen viewport dimensions are capped at 1600 pixels.
Mesh-local and scene-centre rebasing preserve useful f32 rendering precision.
Reading, parsing, pipeline creation, uploads and draw encoding run on workers.

For local GPU and interaction verification, without publishing input or screenshots:

```sh
cargo run -p craft-anything-ui-egui --example scene_snapshot -- /absolute/bundle/scene.json /absolute/private-output
```

The verifier requires a hardware graphics adapter, loads the actual bundle, renders PNGs,
aims a ray at geometry and
exercises Hide, Show all, Isolate and Fit selection through UI buttons. It also
renders after camera orbit, pan and zoom and records explicit verification limits.
It is a test example, so its screenshot writes are outside production UI code.
