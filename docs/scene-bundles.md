# Experimental scene bundles

The native Project viewer opens the admitted **6.5.5.28 `.sp` layout directly**,
or an original **version-1 scene bundle**, with a `scene.json` manifest and six
fixed local members. Use Project viewer in the desktop title bar, open a project
through the normal file picker, or pass its path to the desktop executable.
It shares the workbench engine and its Deep setting; no source application,
external conversion process, AI service or prepared bundle is needed.
Unsupported source versions/layouts fail explicitly. This is experimental,
read-only support for admitted structures, not general format compatibility.

```sh
cargo run -p craft-anything-ui-egui --example scene_viewer -- /absolute/bundle/scene.json
```

Drag to orbit, Shift-drag or right-drag to pan, and scroll to zoom. Click a surface
to select its mesh instance. The resource tree selects a resource; Hide and Isolate
include its descendant resources. Fit selection frames the selected instance or
the geometry of the selected resource and its descendants. Show all resets filters.
Choosing Y up changes the camera only. The renderer preserves stored coordinates;
three independently inspected native leaves agreed in XYZ metres, but this does
not label arbitrary files or engineering measurements as verified. Shading is
two-sided. Admitted explicit entity RGB overrides are retained separately and can
be shown with Object colours. Three native leaf readbacks and a saved/reopened
single-colour control established the binary RGB field; the source application's
original-colour action removes that override payload. Only direct entity colours
are applied: inherited colours and texture precedence remain unresolved and keep
placeholder shading. Optional source diffuse candidates use a declared nearest-explicit-node
policy; material precedence, texture assets and full visual parity are unverified.

3D shows the model. 4D adds focus time, playback, source task dates and assignment
appearance. Calendars determine working finishes; summary dates aggregate children,
milestones remain instants, and actual/best endpoints retain missing-date limits.
Displayed wallclock values undergo no timezone conversion or dependency rescheduling.
Strict appearance supports admitted Install/Maintain intervals. The separately
labelled Interval preview admits custom Install base-kind intervals while omitting
directional growth, transparency and original-material effects. Unknown assignments
remain visible with explicit unresolved results rather than disappearing silently.

5D shows task direct budgets separately from the resource-assignment forecast,
planned/actual/remaining source quantities, and supported Fixed/PerUnit material
costs. Values use checked integer minor amounts and per-allocation rounding.
Planned working-time allocations handle admitted rate changes. Missing actual Fixed costs,
unsupported resource kinds, uncertain rates and invalid dates stay unresolved.
Source quantities are distinct from mesh volume/area; no geometry-based quantity
takeoff or currency conversion is claimed. Select a task to isolate its assigned
resource geometry.

All file loading and surface selection run through engine tools on workers:

| Tool | Result |
|---|---|
| `formats_project_open` | Reads the admitted `.sp` layout into the same typed scene cache, retaining tasks, resources, calendars and storage provenance. |
| `formats_scene_bundle_open` | Validates and retains a bundle; returns its content fingerprint, declared provenance, counts and bounds. |
| `formats_scene_resources_list` | Pages resource GUIDs, names, parent GUIDs and directly associated instance counts. |
| `formats_scene_surface_pick` | Tests a world-space ray against visible triangles; returns the nearest hit, or explicitly incomplete selection. |
| `formats_project_tasks_list` | Pages source tasks and resolved calendar/summary schedule bounds. |
| `formats_project_costs_list` | Pages source resource cost and unit inputs. |
| `formats_project_frame_evaluate` | Returns paged phases, cost lines and interpretation limits, plus unpaged summary totals and explicit completeness counts. |

These tools require Deep mode. They perform no source execution and open no
dialogs. CLI example: `craft-anything-cli --deep formats-scene-bundle-open /absolute/bundle/scene.json`.
Resources and picking use the same retained session as Open (for example, a
long-running MCP session); independent CLI invocations do not share that cache.
Project task/cost/evaluation tools also accept an optional absolute `.sp` `path`
for a single headless call. Evaluation accepts `focus_wallclock`, `date_mode`
(`planned` or `best`), explicit `interval_preview`, and `offset`/`limit` (up to
1,000). A complete bounded typed frame remains in the session for the desktop;
JSON collections are paged. Missing or unresolved values are never silently zeroed.
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
supplies resource identities and the admitted schedule/cost storage fields.
Optional node `source_diffuse` values are finite RGB candidates in [0,1]; older
bundles without them retain placeholder shading.
Optional `source_object_color` values are separate explicit RGB overrides. Their
declared policy is `observed_entity_rgb_override_no_inheritance`; no library or
ancestor fallback is inferred. Versions without these optional fields still open.

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
Camera-frustum culling includes conservative GPU quantization margins. The default
per-frame budget is 50 million expanded triangles; exceeding it returns a recoverable
error instead of drawing an undisclosed subset. The sidebar reports visible,
culled and hidden instances, expanded triangles and measured frame completion time.
Rendering errors/cancellation retain GPU ownership so a corrected view can recover.

For local GPU and interaction verification, without publishing input or screenshots:

```sh
cargo run -p craft-anything-ui-egui --example scene_snapshot -- /absolute/bundle/scene.json /absolute/private-output
```

The verifier requires a hardware graphics adapter, loads the actual project or bundle, renders PNGs,
aims a ray at geometry and
exercises Hide, Show all, Isolate and Fit selection through UI buttons. It also
renders after camera orbit, pan and zoom and records explicit verification limits.
For source projects it also evaluates before/middle/after schedule frames, verifies
appearance changes, and saves 4D, 5D and experimental diffuse images with typed
frame JSON. It is a test example, so screenshot writes are outside production UI code.
