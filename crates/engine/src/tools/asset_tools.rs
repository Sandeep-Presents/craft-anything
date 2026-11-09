//! Game assets the builder generates with fal.ai: `assets_image_generate` (a picture: a texture, a sky,
//! concept art) and `assets_model_generate` (a low-poly GLB model, from a description or a picture). Each one
//! is paid for, so a project has a budget: the builder spends freely under it, and the person is
//! asked before it goes over. Nothing is generated unless the person turned it on for the project
//! (`project_assets_allow`). Every asset and its cost is listed in the project (assets.json).
//! The provider comes from the `AssetProviders` registry and the models and prices from the
//! asset table, so the cost is known before anything is spent.

use crate::inbox::Inbox;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use crate::tools::interaction_tools::{Verdict, ask_approval, inbox, inside, resolve, workspace_of};
use crate::workspace::{AssetRecord, Workspace};
use craft_anything_engines::assets::{AssetProvider, AssetProviders, AssetRequest, Catalogue, Generated, ImageModel, Shape, about_usd, comfyui, fal, usd};
use craft_anything_evidence::{ErrorCode, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The person's key for each asset provider (by provider id), put in the session by the tool
/// server or a build's run (read from the keychain; a key never travels on a command line or into a
/// log).
#[derive(Default)]
pub struct AssetKeys(pub std::collections::BTreeMap<String, String>);

impl AssetKeys {
    /// The key for `provider`, if one is set up.
    pub fn get(&self, provider: &str) -> Option<&str> {
        self.0.get(provider).map(String::as_str).filter(|k| !k.trim().is_empty())
    }
}

/// Remembered when the person says "always" to going over the budget.
const OVER_BUDGET: &str = "assets:over-budget";

pub fn specs() -> Vec<ToolSpec> {
    let effects = Effects::ACCESSES_NETWORK.union(Effects::WRITES_FILESYSTEM);
    vec![
        ToolSpec {
            id: "assets_image_generate",
            title: "Generate a picture",
            summary: &summaries().0,
            beginner: "Makes a picture for your game, like a texture or a sky.",
            toolset: Toolset::Core,
            effects,
            requires: Requires::Nothing,
            input_schema: schema_of::<ImageParams>,
            run: assets_image_generate,
        },
        ToolSpec {
            id: "assets_model_generate",
            title: "Generate a 3D model",
            summary: &summaries().1,
            beginner: "Makes a 3D object for your game, like a crate, a tree or a car.",
            toolset: Toolset::Core,
            effects,
            requires: Requires::Nothing,
            input_schema: schema_of::<ModelParams>,
            run: assets_model_generate,
        },
        ToolSpec {
            id: "assets_comfyui_check",
            title: "Check a ComfyUI server",
            summary: "Ask a ComfyUI server (the address given, else the one in Settings → Builds) what it is and which checkpoints it has: its version, its graphics cards and the checkpoint names a picture can be made with. Needs no key.",
            beginner: "Checks that your ComfyUI server is running and lists the models it can make pictures with.",
            toolset: Toolset::Core,
            effects: Effects::ACCESSES_NETWORK,
            requires: Requires::Nothing,
            input_schema: schema_of::<ComfyCheckParams>,
            run: assets_comfyui_check,
        },
    ]
}

/// The two tools' summaries, with the prices and face limits of the asset table's default models.
fn summaries() -> &'static (String, String) {
    static TEXT: OnceLock<(String, String)> = OnceLock::new();
    TEXT.get_or_init(|| {
        let c = Catalogue::built_in();
        let (picture, picture_model) = c.image_model(None).map(|m| (m.cost(Shape::Square), m.name.as_str())).unwrap_or((0.0, "the default model"));
        let (textured, faces) = c.model_3d(None).map(|m| (m.cost(true), m.faces())).unwrap_or((0.0, 48..=20_000));
        let image = format!(
            "Make a picture with fal.ai ({picture_model}) and save it as a PNG inside app/: a texture, a sky or background, an icon, a title screen or concept art. shape: square (1024x1024, default), wide (1024x576) or tall (576x1024). Costs {}, counted against the project's asset budget. Describe original designs in the original's style; never ask for recognisable characters, logos or brand artwork. Returns where it was saved, the cost and the budget left.",
            about_usd(picture)
        );
        let model = format!(
            "Make a game-ready, low-poly 3D model (GLB) with fal.ai (Tripo P1) and save it inside app/. Give a prompt (a picture is made first, then turned into the model) or an image (a web address, or a picture inside the project such as reference/picture-1.png). faces caps the face count ({} to {}, default {DEFAULT_FACES}): about 500-1500 for small props and pickups, 2000-5000 for weapons, furniture and vehicles, 6000-15000 for main characters; fewer faces run faster, so use the fewest that still look right. textured (default true) gives colours and surfaces. A textured model costs {} plus {} for the picture, counted against the project's asset budget; it takes a minute or two. Describe one original object per model; never ask for recognisable characters, logos or brand artwork. Returns where it was saved, the cost and the budget left.",
            faces.start(),
            faces.end(),
            usd(textured),
            usd(picture)
        );
        (image, model)
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageParams {
    /// What to draw, in detail (subject, style, colours, lighting).
    pub prompt: String,
    /// Where to save it, relative to app/, ending in .png (e.g. Assets/Textures/crate.png).
    pub save_as: String,
    /// square (default), wide or tall.
    #[serde(default)]
    pub shape: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelParams {
    /// Where to save it, relative to app/, ending in .glb (e.g. Assets/Models/crate.glb).
    pub save_as: String,
    /// What to make: one object, described in detail. Used when no image is given.
    #[serde(default)]
    pub prompt: String,
    /// A picture to turn into the model: a web address, or a picture inside the project.
    #[serde(default)]
    pub image: String,
    /// Colours and surfaces (default true); false makes a plain grey model for $0.40 instead of $0.50.
    #[serde(default)]
    pub textured: Option<bool>,
    /// Most faces the model may have, 48 to 20000 (default 3000). Fewer faces run faster.
    #[serde(default)]
    pub faces: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComfyCheckParams {
    /// The server's address (http://host:port); empty = the one in Settings.
    #[serde(default)]
    pub address: Option<String>,
}

/// A mid-sized game prop: detailed enough up close, cheap to draw many of.
const DEFAULT_FACES: u32 = 3000;

/// What a generation needs: the keys, the providers, the inbox (for the over-budget question) and
/// the project.
struct Job {
    keys: std::sync::Arc<AssetKeys>,
    providers: AssetProviders,
    inbox: Inbox,
    ws: Workspace,
}

fn job(ctx: &Ctx<'_>) -> Result<Job, ToolError> {
    // First: outside a project no keys are loaded, so "no key" would be wrong for a saved one (#923).
    let not_project = || ToolError::new(ErrorCode::FeatureMissing, "Assets can only be generated inside a Craft Anything project.");
    let inbox = inbox(ctx).ok_or_else(not_project)?;
    let ws = workspace_of(&inbox).ok_or_else(not_project)?;
    let keys = ctx.session.ext.get::<AssetKeys>().unwrap_or_default();
    // A session can bring its own providers (a fork, a test); otherwise the built-in ones.
    let mut providers = ctx.session.ext.get::<AssetProviders>().map(|p| p.as_ref().clone()).unwrap_or_default();
    // The person's ComfyUI server, when one is set up: it needs no key.
    let comfy = &ctx.session.settings.assets.comfyui;
    if !comfy.address.trim().is_empty() {
        providers.register(std::sync::Arc::new(comfyui::ComfyUi::new(
            comfy.address.trim(),
            comfy.checkpoint.trim(),
            comfy.workflow.as_deref().map(str::trim).filter(|w| !w.is_empty()).map(PathBuf::from),
        )));
    }
    // No way to make anything (no saved key, no ComfyUI server): say so before the permission check (#923).
    // ComfyUI chosen as the picture model says what it's missing itself, so it passes here.
    let comfy_chosen = ctx.session.settings.assets.image_model.as_deref() == Some(comfyui::ID);
    if !comfy_chosen && !providers.ids().into_iter().any(|id| keys.get(id).is_some() || providers.get(id).is_some_and(|p| !p.needs_key())) {
        return Err(no_key("fal.ai"));
    }
    if !ws.manifest.generate_assets {
        return Err(ToolError::new(ErrorCode::NoPermission, "The person hasn't turned on generated assets for this project.").with_remediation(
            "Make these assets with simple shapes and colours instead, and tell the person they can turn on generated assets for this game in the app.",
        ));
    }
    Ok(Job { keys, providers, inbox, ws })
}

/// ComfyUI chosen but not set up: say where to set it up, before anything is asked.
fn comfy_ready(s: &crate::settings::ComfyUiSettings, model: &ImageModel) -> Result<(), ToolError> {
    if model.provider != comfyui::ID {
        return Ok(());
    }
    let missing = if s.address.trim().is_empty() {
        "address"
    } else if s.checkpoint.trim().is_empty() {
        "checkpoint"
    } else {
        return Ok(());
    };
    Err(ToolError::new(ErrorCode::BadSettings, format!("The ComfyUI picture model has no {missing} set.")).with_remediation(
        "Make these assets with simple shapes and colours instead, and tell the person they can set the ComfyUI address and checkpoint in Settings → Builds.",
    ))
}

fn no_key(provider: &str) -> ToolError {
    ToolError::new(ErrorCode::BadSettings, format!("No {provider} key is set up, so pictures and 3D models can't be generated.")).with_remediation(format!(
        "Make these assets with simple shapes and colours instead, and tell the person they can add a {provider} key in Settings → AI for realistic ones."
    ))
}

impl Job {
    /// The key for `provider`, checked before anything is asked or spent. A provider that needs
    /// none (a server of the person's own) gets "".
    fn key(&self, provider: &dyn AssetProvider) -> Result<String, ToolError> {
        if !provider.needs_key() {
            return Ok(String::new());
        }
        self.keys.get(provider.id()).map(str::to_string).ok_or_else(|| no_key(provider.name()))
    }
}

/// Where to save inside app/: (full path, path relative to app/). `ext` is the required extension.
fn save_path(ws: &Workspace, save_as: &str, ext: &str) -> Result<(PathBuf, String), ToolError> {
    let app = ws.app_dir();
    // Paths given from the workspace ("app/Assets/x.glb") mean the same place.
    let rel = save_as.trim().trim_start_matches("./").trim_start_matches("app/").trim_start_matches("app\\");
    let path = resolve(&app, rel).filter(|p| inside(&app, p) && *p != app).ok_or_else(|| ToolError::invalid("save_as must be a file path inside app/"))?;
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
        return Err(ToolError::invalid(format!("save_as must end in .{ext}")));
    }
    let shown = path.strip_prefix(&app).unwrap_or(&path).display().to_string().replace('\\', "/");
    Ok((path, shown))
}

/// Go ahead if this stays within the budget, else ask the person (unless they said "always").
fn within_budget(ctx: &mut Ctx<'_>, inbox: &Inbox, ws: &mut Workspace, cost: f64) -> Result<(), ToolError> {
    let spent = ws.assets_spent();
    let budget = ws.manifest.asset_budget_usd;
    if spent + cost <= budget + 1e-9 || ws.manifest.allowed_actions.iter().any(|k| k == OVER_BUDGET) {
        return Ok(());
    }
    // Auto mode doesn't stop to ask, and doesn't spend the person's money past the budget unasked.
    if ctx.session.settings.auto_mode {
        return Err(ToolError::new(ErrorCode::OverLimit, format!("This project's ${budget:.2} asset budget is used up (${spent:.2} so far)."))
            .with_remediation("Use simple shapes, colours and the assets already made for the rest. The person can raise the budget in the app."));
    }
    let title = format!("Spend about ${cost:.2} more on game pictures and 3D models?");
    let detail = format!(
        "This project's ${budget:.2} budget for generated assets is used up (${spent:.2} so far). Allow once for this asset, or always to let the builder keep generating for this project."
    );
    match ask_approval(ctx, inbox, Some(ws), &title, &detail, OVER_BUDGET)? {
        Verdict::Yes => Ok(()),
        Verdict::No(_) | Verdict::NoAnswer => Err(ToolError::new(ErrorCode::OverLimit, "The asset budget is used up and the person didn't raise it.")
            .with_remediation("Use simple shapes, colours and the assets already made for the rest.")),
    }
}

/// Save a generated file and list it with its cost.
fn keep(ws: &Workspace, path: &Path, rel: &str, kind: &str, made_from: &str, g: &Generated) -> Result<(), ToolError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ToolError::io(parent.display(), e))?;
    }
    crate::project::write_atomic(path, &g.bytes)?;
    ws.record_asset(AssetRecord {
        file: rel.to_string(),
        kind: kind.into(),
        prompt: made_from.chars().take(1000).collect(),
        cost_usd: g.cost_usd,
        at_ms: crate::inbox::now_ms(),
    })
}

fn summary(ws: &Workspace, saved: &str, cost: f64, extra: Value) -> ToolOutput {
    let spent = ws.assets_spent();
    let mut v =
        json!({"saved": saved, "cost_usd": round(cost), "spent_usd": round(spent), "budget_left_usd": round((ws.manifest.asset_budget_usd - spent).max(0.0))});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    ToolOutput::Value(v)
}

fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn prompt_ok(p: &str) -> Result<&str, ToolError> {
    let p = p.trim();
    if p.is_empty() || p.len() > 2000 {
        return Err(ToolError::invalid("prompt must be 1..=2000 characters"));
    }
    Ok(p)
}

fn assets_image_generate(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ImageParams = parse_params(params)?;
    let prompt = prompt_ok(&p.prompt)?;
    let shape = match p.shape.as_deref() {
        None => Shape::Square,
        Some(s) => Shape::parse(s).ok_or_else(|| ToolError::invalid("shape must be square, wide or tall"))?,
    };
    let mut j = job(ctx)?;
    let (path, rel) = save_path(&j.ws, &p.save_as, "png")?;
    let model = Catalogue::built_in().image_model(ctx.session.settings.assets.image_model.as_deref())?;
    comfy_ready(&ctx.session.settings.assets.comfyui, model)?;
    let req = AssetRequest::Image { model, prompt, shape };
    let provider = j.providers.for_request(&req)?;
    let key = j.key(provider.as_ref())?;
    within_budget(ctx, &j.inbox, &mut j.ws, provider.estimate(&req)?)?;
    ctx.report(None, "Making a picture…");
    let g = provider.generate(&key, &req)?;
    keep(&j.ws, &path, &rel, "image", prompt, &g)?;
    Ok(summary(&j.ws, &rel, g.cost_usd, json!({})))
}

fn assets_comfyui_check(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ComfyCheckParams = parse_params(params)?;
    let given = p.address.as_deref().map(str::trim).filter(|a| !a.is_empty());
    let address = given.unwrap_or(ctx.session.settings.assets.comfyui.address.trim()).to_string();
    if address.is_empty() {
        return Err(ToolError::new(ErrorCode::BadSettings, "No ComfyUI address is set.")
            .with_remediation("Give the address, for example http://192.168.1.20:8188, in Settings → Builds → Picture model."));
    }
    ctx.report(None, "Asking the ComfyUI server…");
    let status = comfyui::check(&address)?;
    Ok(ToolOutput::Value(json!({
        "version": status.version,
        "devices": status.devices,
        "vram_gb": status.vram_gb,
        "checkpoints": status.checkpoints,
        "words": status.words(),
    })))
}

fn assets_model_generate(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ModelParams = parse_params(params)?;
    let textured = p.textured.unwrap_or(true);
    let faces = p.faces.unwrap_or(DEFAULT_FACES);
    let catalogue = Catalogue::built_in();
    let model = catalogue.model_3d(None)?;
    let range = model.faces();
    if !range.contains(&faces) {
        return Err(ToolError::invalid(format!("faces must be {} to {}", range.start(), range.end())));
    }
    let mut j = job(ctx)?;
    let (path, rel) = save_path(&j.ws, &p.save_as, "glb")?;
    let image = p.image.trim();
    let from_prompt = image.is_empty();
    // The picture's address isn't known yet; the estimate doesn't depend on it.
    let model_req = AssetRequest::Model3d { model, image_url: "", textured, faces };
    let provider = j.providers.for_request(&model_req)?;
    let key = j.key(provider.as_ref())?;
    let mut estimate = provider.estimate(&model_req)?;
    // From a prompt, a picture is made first: with the picture model chosen in Settings.
    let mut picture_maker = None;
    if from_prompt {
        let image_model = catalogue.image_model(ctx.session.settings.assets.image_model.as_deref())?;
        comfy_ready(&ctx.session.settings.assets.comfyui, image_model)?;
        let picture_req = AssetRequest::Image { model: image_model, prompt: &p.prompt, shape: Shape::Square };
        let picture_provider = j.providers.for_request(&picture_req)?;
        let picture_key = j.key(picture_provider.as_ref())?;
        estimate += picture_provider.estimate(&picture_req)?;
        picture_maker = Some((image_model, picture_provider, picture_key));
    }
    let source = if from_prompt { None } else { Some(image_source(&j.ws, image)?) };
    within_budget(ctx, &j.inbox, &mut j.ws, estimate)?;
    let mut cost = 0.0;
    let mut picture = Value::Null;
    let (url, made_from) = match source {
        Some(src) => (src, format!("from {image}")),
        None => {
            let prompt = prompt_ok(&p.prompt).map_err(|_| ToolError::invalid("give a prompt (what to make) or an image to make it from"))?;
            let (image_model, picture_provider, picture_key) = picture_maker.ok_or_else(|| ToolError::internal("no picture model was chosen"))?;
            ctx.report(None, "Making a picture of it first…");
            // Image-to-3D works best from one whole object on a plain background.
            let described = format!(
                "{prompt}. One whole object, centred and fully in view, on a plain light-grey background, soft even lighting, three-quarter view, no text."
            );
            let g = picture_provider.generate(&picture_key, &AssetRequest::Image { model: image_model, prompt: &described, shape: Shape::Square })?;
            let (pic_path, pic_rel) = save_path(&j.ws, &format!("{}.png", rel.trim_end_matches(".glb").trim_end_matches(".GLB")), "png")?;
            keep(&j.ws, &pic_path, &pic_rel, "image", prompt, &g)?;
            cost += g.cost_usd;
            picture = Value::String(pic_rel);
            (g.url, prompt.to_string())
        }
    };
    ctx.report(None, "Turning it into a 3D model (a minute or two)…");
    let g = provider.generate(&key, &AssetRequest::Model3d { model, image_url: &url, textured, faces })?;
    keep(&j.ws, &path, &rel, "model", &made_from, &g)?;
    cost += g.cost_usd;
    Ok(summary(&j.ws, &rel, cost, json!({"picture": picture, "textured": textured, "faces": faces})))
}

/// The picture a model is made from: a web address as it is, or a picture in the project sent
/// inline.
fn image_source(ws: &Workspace, image: &str) -> Result<String, ToolError> {
    if image.starts_with("https://") || image.starts_with("http://") {
        return Ok(image.to_string());
    }
    let path = resolve(&ws.root, image)
        .filter(|p| inside(&ws.root, p))
        .ok_or_else(|| ToolError::invalid("image must be a web address or a picture inside the project"))?;
    let media = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => return Err(ToolError::invalid("image must be a PNG, JPEG or WebP picture")),
    };
    let meta = std::fs::metadata(&path).map_err(|e| ToolError::io(path.display(), e))?;
    if meta.len() > 20 << 20 {
        return Err(ToolError::invalid("that picture is over 20 MB"));
    }
    let bytes = std::fs::read(&path).map_err(|e| ToolError::io(path.display(), e))?;
    Ok(fal::data_uri(&bytes, media))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::{Answer, InboxDir};
    use crate::workspace::TargetRef;
    use std::time::Duration;

    /// A game whose person allows generated assets.
    fn ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let mut w = Workspace::create(dir.path(), "game", "A snake game", TargetRef::None).unwrap();
        w.set_person(|m| m.generate_assets = true).unwrap();
        (dir, w)
    }

    #[test]
    fn saved_inside_app_with_the_right_ending() {
        let (_d, w) = ws();
        let (p, rel) = save_path(&w, "Assets/Models/crate.glb", "glb").unwrap();
        assert_eq!(rel, "Assets/Models/crate.glb");
        assert!(p.starts_with(w.app_dir()));
        assert_eq!(save_path(&w, "app/Assets/sky.png", "png").unwrap().1, "Assets/sky.png", "paths from the workspace mean the same place");
        assert!(save_path(&w, "Assets/crate.png", "glb").is_err(), "the ending must match");
        assert!(save_path(&w, "../../evil.glb", "glb").is_err(), "nothing outside app/");
        assert!(image_source(&w, "../../../secret.png").is_err(), "pictures come from the project or the web");
        assert_eq!(image_source(&w, "https://v3.fal.media/x.png").unwrap(), "https://v3.fal.media/x.png");
    }

    #[test]
    fn the_summaries_read_the_asset_table() {
        // The default picture model's name and price, and the default 3D model's.
        assert_eq!(
            summaries().0,
            "Make a picture with fal.ai (Nano Banana Pro) and save it as a PNG inside app/: a texture, a sky or background, an icon, a title screen or concept art. shape: square (1024x1024, default), wide (1024x576) or tall (576x1024). Costs about $0.15, counted against the project's asset budget. Describe original designs in the original's style; never ask for recognisable characters, logos or brand artwork. Returns where it was saved, the cost and the budget left."
        );
        assert_eq!(
            summaries().1,
            "Make a game-ready, low-poly 3D model (GLB) with fal.ai (Tripo P1) and save it inside app/. Give a prompt (a picture is made first, then turned into the model) or an image (a web address, or a picture inside the project such as reference/picture-1.png). faces caps the face count (48 to 20000, default 3000): about 500-1500 for small props and pickups, 2000-5000 for weapons, furniture and vehicles, 6000-15000 for main characters; fewer faces run faster, so use the fewest that still look right. textured (default true) gives colours and surfaces. A textured model costs $0.50 plus $0.15 for the picture, counted against the project's asset budget; it takes a minute or two. Describe one original object per model; never ask for recognisable characters, logos or brand artwork. Returns where it was saved, the cost and the budget left."
        );
    }

    /// Stands in for fal.ai: records each request's endpoint and answers with a few bytes.
    #[derive(Default)]
    struct StandIn(craft_anything_platform::sync::Mutex<Vec<String>>);

    impl craft_anything_engines::assets::AssetProvider for StandIn {
        fn id(&self) -> &'static str {
            "fal"
        }
        fn name(&self) -> &'static str {
            "stand-in"
        }
        fn generate(&self, key: &str, req: &AssetRequest) -> Result<Generated, ToolError> {
            assert_eq!(key, "k-1");
            let (endpoint, bytes) = match req {
                AssetRequest::Image { model, .. } => (model.endpoint.clone(), b"png".to_vec()),
                AssetRequest::Model3d { model, image_url, .. } => {
                    assert_eq!(*image_url, "https://stand-in/picture.png", "the model is made from the picture");
                    (model.endpoint.clone(), b"glb".to_vec())
                }
                _ => return Err(ToolError::invalid("the stand-in makes pictures and 3D models only")),
            };
            self.0.lock().unwrap().push(endpoint);
            Ok(Generated { url: "https://stand-in/picture.png".into(), bytes, cost_usd: req.listed_cost() })
        }
    }

    #[test]
    fn a_model_from_a_prompt_through_the_provider_registry() {
        let (_d, w) = ws();
        let stand_in = std::sync::Arc::new(StandIn::default());
        let mut providers = AssetProviders::built_in();
        providers.register(stand_in.clone());
        let mut session = crate::testing::session();
        session.settings.assets.image_model = Some("flux-schnell".into());
        let e = crate::Engine::new(session);
        e.session().ext.insert(std::sync::Arc::new(InboxDir(w.inbox_dir())));
        e.session().ext.insert(std::sync::Arc::new(AssetKeys([("fal".to_string(), "k-1".to_string())].into())));
        e.session().ext.insert(std::sync::Arc::new(providers));
        let out =
            e.call("assets_model_generate", json!({"prompt": "a crate", "save_as": "Assets/crate.glb"}), crate::CallOptions::new(crate::Caller::Mcp)).unwrap();
        let ToolOutput::Value(v) = out else { panic!("{out:?}") };
        assert_eq!(v["saved"], "Assets/crate.glb");
        assert_eq!(v["picture"], "Assets/crate.png");
        assert_eq!(*stand_in.0.lock().unwrap(), vec!["fal-ai/flux/schnell".to_string(), "tripo3d/p1/image-to-3d".to_string()], "the configured picture model");
        assert_eq!(std::fs::read(w.app_dir().join("Assets/crate.glb")).unwrap(), b"glb");
        let spent = w.assets_spent();
        assert!((spent - (0.50 + 0.003 * 1.048576)).abs() < 1e-9, "{spent}");
        assert_eq!(w.assets().len(), 2);
    }

    #[test]
    fn a_key_for_another_provider_doesnt_pay_this_one() {
        let (_d, w) = ws();
        let e = crate::Engine::new(crate::testing::session());
        e.session().ext.insert(std::sync::Arc::new(InboxDir(w.inbox_dir())));
        e.session().ext.insert(std::sync::Arc::new(AssetKeys([("fal".to_string(), " ".to_string())].into())));
        let err =
            e.call("assets_image_generate", json!({"prompt": "a crate", "save_as": "crate.png"}), crate::CallOptions::new(crate::Caller::Mcp)).err().unwrap();
        assert!(err.code == ErrorCode::BadSettings, "a blank key is no key");
        let mut providers = AssetProviders::built_in();
        providers.register(std::sync::Arc::new(Named("other")));
        e.session().ext.insert(std::sync::Arc::new(providers));
        e.session().ext.insert(std::sync::Arc::new(AssetKeys([("other".to_string(), "o-1".to_string())].into())));
        let err =
            e.call("assets_image_generate", json!({"prompt": "a crate", "save_as": "crate.png"}), crate::CallOptions::new(crate::Caller::Mcp)).err().unwrap();
        assert_eq!(err.code, ErrorCode::BadSettings);
        assert!(err.message.contains("No fal.ai key"), "{}", err.message);
        assert!(w.assets().is_empty() && Inbox::new(w.inbox_dir()).pending().is_empty(), "nothing asked or spent");
    }

    /// A provider that serves nothing in the catalogue.
    struct Named(&'static str);

    impl craft_anything_engines::assets::AssetProvider for Named {
        fn id(&self) -> &'static str {
            self.0
        }
        fn name(&self) -> &'static str {
            self.0
        }
        fn generate(&self, _key: &str, _req: &AssetRequest) -> Result<Generated, ToolError> {
            Err(ToolError::invalid("not here"))
        }
    }

    /// #923: the CLI and a plain tool server load no keys, and "no key" sent people to add one they had.
    #[test]
    fn outside_a_project_it_says_so_and_not_that_there_is_no_key() {
        let e = crate::Engine::new(crate::testing::session());
        let err =
            e.call("assets_image_generate", json!({"prompt": "a crate", "save_as": "crate.png"}), crate::CallOptions::new(crate::Caller::Cli)).err().unwrap();
        assert_eq!(err.code, ErrorCode::FeatureMissing);
        assert!(err.message.contains("inside a Craft Anything project"), "{}", err.message);
    }

    #[test]
    fn without_a_key_it_says_to_use_simple_shapes() {
        let (_d, w) = ws();
        let e = crate::Engine::new(crate::testing::session());
        e.session().ext.insert(std::sync::Arc::new(InboxDir(w.inbox_dir())));
        let err =
            e.call("assets_image_generate", json!({"prompt": "a crate", "save_as": "crate.png"}), crate::CallOptions::new(crate::Caller::Mcp)).err().unwrap();
        assert_eq!(err.code, ErrorCode::BadSettings);
        assert!(err.remediation.unwrap_or_default().contains("simple shapes"));
        let err =
            e.call("assets_model_generate", json!({"prompt": "a crate", "save_as": "crate.glb", "faces": 40_000}), crate::CallOptions::new(crate::Caller::Mcp));
        assert!(err.err().unwrap().message.contains("faces must be 48 to 20000"), "Tripo P1's face limit");
    }

    /// A stand-in ComfyUI server: accepts any job, finishes it at once and serves a few bytes.
    fn comfy_server() -> String {
        use std::io::{BufRead, BufReader, Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in l.incoming().flatten() {
                let mut r = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if h == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; len];
                r.read_exact(&mut body).unwrap();
                let (kind, out): (&str, Vec<u8>) = if line.starts_with("POST /prompt") {
                    ("application/json", json!({"prompt_id": "p1"}).to_string().into_bytes())
                } else if line.starts_with("GET /history/p1") {
                    let image = json!({"filename": "a.png", "subfolder": "", "type": "output"});
                    (
                        "application/json",
                        json!({"p1": {"status": {"status_str": "success", "completed": true}, "outputs": {"7": {"images": [image]}}}}).to_string().into_bytes(),
                    )
                } else if line.starts_with("GET /view") {
                    ("image/png", b"comfy-png".to_vec())
                } else if line.starts_with("GET /system_stats") {
                    (
                        "application/json",
                        json!({"system": {"comfyui_version": "0.3.10"}, "devices": [{"name": "cuda:0 NVIDIA GeForce RTX 3060 : cudaMallocAsync"}]})
                            .to_string()
                            .into_bytes(),
                    )
                } else if line.starts_with("GET /object_info/CheckpointLoaderSimple") {
                    (
                        "application/json",
                        json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["a.safetensors"], {}]}}}}).to_string().into_bytes(),
                    )
                } else {
                    ("text/plain", b"no".to_vec())
                };
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                let _ = stream.write_all(&out);
            }
        });
        base
    }

    /// An engine for a project that may generate assets, with no key of any kind, set to make
    /// pictures with ComfyUI at `address` (checkpoint `checkpoint`).
    fn comfy_engine(w: &Workspace, address: &str, checkpoint: &str) -> crate::Engine {
        let mut session = crate::testing::session();
        session.settings.assets.image_model = Some("comfyui".into());
        session.settings.assets.comfyui.address = address.into();
        session.settings.assets.comfyui.checkpoint = checkpoint.into();
        let e = crate::Engine::new(session);
        e.session().ext.insert(std::sync::Arc::new(InboxDir(w.inbox_dir())));
        e
    }

    #[test]
    fn a_picture_from_comfyui_needs_no_key_and_costs_nothing() {
        let (_d, w) = ws();
        let e = comfy_engine(&w, &comfy_server(), "a.safetensors");
        let out =
            e.call("assets_image_generate", json!({"prompt": "a crate", "save_as": "Assets/crate.png"}), crate::CallOptions::new(crate::Caller::Mcp)).unwrap();
        let ToolOutput::Value(v) = out else { panic!("{out:?}") };
        assert_eq!((v["saved"].as_str(), v["cost_usd"].as_f64()), (Some("Assets/crate.png"), Some(0.0)));
        assert_eq!(std::fs::read(w.app_dir().join("Assets/crate.png")).unwrap(), b"comfy-png");
        assert!(w.assets_spent().abs() < 1e-12 && w.assets().len() == 1, "listed, at no cost");
    }

    /// The gate is per provider: the same project with a key for nothing still refuses fal.ai's pictures.
    #[test]
    fn the_keyless_pass_is_only_for_a_provider_that_needs_no_key() {
        let (_d, w) = ws();
        let mut session = crate::testing::session();
        session.settings.assets.comfyui.address = "http://127.0.0.1:9".into();
        session.settings.assets.comfyui.checkpoint = "a.safetensors".into();
        let e = crate::Engine::new(session);
        e.session().ext.insert(std::sync::Arc::new(InboxDir(w.inbox_dir())));
        // The picture model is the default (fal.ai): a ComfyUI address being set is no reason to go on without a key.
        let err =
            e.call("assets_image_generate", json!({"prompt": "a crate", "save_as": "crate.png"}), crate::CallOptions::new(crate::Caller::Mcp)).err().unwrap();
        assert_eq!(err.code, ErrorCode::BadSettings);
        assert!(err.message.contains("No fal.ai key"), "{}", err.message);
    }

    #[test]
    fn comfyui_chosen_but_not_set_up_points_at_settings() {
        let (_d, w) = ws();
        for (address, checkpoint, missing) in [("", "a.safetensors", "address"), ("http://127.0.0.1:9", " ", "checkpoint")] {
            let e = comfy_engine(&w, address, checkpoint);
            let err = e
                .call("assets_image_generate", json!({"prompt": "a crate", "save_as": "crate.png"}), crate::CallOptions::new(crate::Caller::Mcp))
                .err()
                .unwrap();
            assert_eq!(err.code, ErrorCode::BadSettings);
            assert!(err.message.contains(missing), "{}", err.message);
            assert!(err.remediation.unwrap_or_default().contains("Settings → Builds"));
            e.session().ext.insert(std::sync::Arc::new(AssetKeys([("fal".to_string(), "k-1".to_string())].into())));
            let err = e
                .call("assets_model_generate", json!({"prompt": "a crate", "save_as": "crate.glb"}), crate::CallOptions::new(crate::Caller::Mcp))
                .err()
                .unwrap();
            assert!(err.message.contains(missing), "the picture step of a model says so too: {}", err.message);
        }
        assert!(w.assets().is_empty(), "nothing spent");
    }

    /// Makes the 3D model from the picture a server of the person's own made: sent as data, since
    /// the server's address means nothing to fal.ai.
    struct Model3dFromData;

    impl craft_anything_engines::assets::AssetProvider for Model3dFromData {
        fn id(&self) -> &'static str {
            "fal"
        }
        fn name(&self) -> &'static str {
            "stand-in"
        }
        fn generate(&self, _key: &str, req: &AssetRequest) -> Result<Generated, ToolError> {
            let AssetRequest::Model3d { image_url, .. } = req else { return Err(ToolError::invalid("3D only")) };
            assert!(image_url.starts_with("data:image/png;base64,"), "{image_url}");
            Ok(Generated { url: String::new(), bytes: b"glb".to_vec(), cost_usd: req.listed_cost() })
        }
    }

    #[test]
    fn the_picture_step_of_a_3d_model_can_be_comfyui() {
        let (_d, w) = ws();
        let e = comfy_engine(&w, &comfy_server(), "a.safetensors");
        let mut providers = AssetProviders::built_in();
        providers.register(std::sync::Arc::new(Model3dFromData));
        e.session().ext.insert(std::sync::Arc::new(providers));
        e.session().ext.insert(std::sync::Arc::new(AssetKeys([("fal".to_string(), "k-1".to_string())].into())));
        let out =
            e.call("assets_model_generate", json!({"prompt": "a crate", "save_as": "Assets/crate.glb"}), crate::CallOptions::new(crate::Caller::Mcp)).unwrap();
        let ToolOutput::Value(v) = out else { panic!("{out:?}") };
        assert_eq!(v["picture"], "Assets/crate.png");
        assert_eq!(std::fs::read(w.app_dir().join("Assets/crate.png")).unwrap(), b"comfy-png");
        assert!((w.assets_spent() - 0.50).abs() < 1e-9, "the picture was free, the model wasn't: {}", w.assets_spent());
    }

    #[test]
    fn the_comfyui_check_tool_reads_the_server_and_falls_back_to_settings() {
        let (_d, w) = ws();
        let base = comfy_server();
        let e = comfy_engine(&w, &base, "");
        for params in [json!({}), json!({"address": base})] {
            let ToolOutput::Value(v) = e.call("assets_comfyui_check", params, crate::CallOptions::new(crate::Caller::Mcp)).unwrap() else { panic!() };
            assert_eq!(
                (v["version"].as_str(), v["checkpoints"][0].as_str(), v["devices"][0].as_str()),
                (Some("0.3.10"), Some("a.safetensors"), Some("NVIDIA GeForce RTX 3060"))
            );
            assert_eq!(v["words"], "ComfyUI 0.3.10 · NVIDIA GeForce RTX 3060 · 1 checkpoint");
        }
        let none = crate::Engine::new(crate::testing::session());
        let err = none.call("assets_comfyui_check", json!({}), crate::CallOptions::new(crate::Caller::Mcp)).err().unwrap();
        assert_eq!(err.code, ErrorCode::BadSettings);
    }

    #[test]
    fn nothing_is_generated_unless_the_project_allows_it() {
        let (_d, mut w) = ws();
        w.set_person(|m| m.generate_assets = false).unwrap();
        let e = crate::Engine::new(crate::testing::session());
        e.session().ext.insert(std::sync::Arc::new(InboxDir(w.inbox_dir())));
        e.session().ext.insert(std::sync::Arc::new(AssetKeys([("fal".to_string(), "k-1".to_string())].into())));
        let err =
            e.call("assets_image_generate", json!({"prompt": "a crate", "save_as": "crate.png"}), crate::CallOptions::new(crate::Caller::Mcp)).err().unwrap();
        assert_eq!(err.code, ErrorCode::NoPermission, "a key alone isn't the person's choice");
        assert!(err.remediation.unwrap_or_default().contains("simple shapes"));
        assert!(w.assets().is_empty() && Inbox::new(w.inbox_dir()).pending().is_empty(), "nothing asked or spent");
    }

    #[test]
    fn the_budget_asks_before_going_over_and_remembers_always() {
        let (_d, mut w) = ws();
        w.record_asset(AssetRecord { file: "a.glb".into(), kind: "model".into(), prompt: "x".into(), cost_usd: 4.80, at_ms: 0 }).unwrap();
        let inbox = Inbox::new(w.inbox_dir());
        let mut session = crate::testing::session();
        // Auto mode stops at the budget without asking.
        session.settings.auto_mode = true;
        {
            let mut ctx = Ctx { tool: "test", session: &mut session, cancel: crate::CancelToken::default(), progress: &|_| {}, caller: crate::Caller::Mcp };
            assert_eq!(within_budget(&mut ctx, &inbox, &mut w, 0.48).unwrap_err().code, ErrorCode::OverLimit);
            assert!(inbox.pending().is_empty(), "no question in Auto mode");
        }
        // Asking is what Auto mode switches off.
        session.settings.auto_mode = false;
        let mut ctx = Ctx { tool: "test", session: &mut session, cancel: crate::CancelToken::default(), progress: &|_| {}, caller: crate::Caller::Mcp };
        // Under the budget: no question.
        within_budget(&mut ctx, &inbox, &mut w, 0.03).unwrap();
        assert!(inbox.pending().is_empty());
        // Over it: asked; "no" stops, "always" lifts the cap for the project.
        let dir = w.inbox_dir();
        let answer = |allow: bool, always: bool| {
            let inbox = Inbox::new(dir.clone());
            std::thread::spawn(move || {
                for _ in 0..200 {
                    if let Some(r) = inbox.pending().first() {
                        assert!(r.title.contains("$0.48"), "{}", r.title);
                        inbox.answer(&r.id, &Answer { allow: Some(allow), always, ..Default::default() }).unwrap();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                panic!("no question came");
            })
        };
        let t = answer(false, false);
        assert_eq!(within_budget(&mut ctx, &inbox, &mut w, 0.48).unwrap_err().code, ErrorCode::OverLimit);
        t.join().unwrap();
        let t = answer(true, true);
        within_budget(&mut ctx, &inbox, &mut w, 0.48).unwrap();
        t.join().unwrap();
        assert!(Workspace::open(&w.root).unwrap().manifest.allowed_actions.iter().any(|k| k == OVER_BUDGET));
        within_budget(&mut ctx, &inbox, &mut w, 10.0).unwrap();
        assert!(inbox.pending().is_empty(), "after \"always\" it doesn't ask again");
        assert!((w.assets_spent() - 4.80).abs() < 1e-9);
    }
}
