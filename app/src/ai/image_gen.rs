//! Making pictures: what the agents' `generate_image` tool and the `imagine` command go through.
//!
//! A request is tried on the backends in turn until one makes a picture, and every failure is
//! reported together if none does:
//!
//! - **The local command**: a program that takes `imagine.py`'s flags and prints one JSON line
//!   with the saved `path` (or an `error`). It is the setting `image_command`, else the
//!   environment variable `KRUST_IMAGE_COMMAND`, else `~/github/image-lab/bin/imagine-engine`. This is
//!   how a local diffusion model is reached (FLUX.2 klein run with diffusers, say): Ollama
//!   removed its image generation in 0.32.6, so its image models cannot be used there.
//! - **Models in the agent pool that make images** (capability `image_out`, see
//!   [`crate::ai::model_caps`]): a cloud provider's or custom endpoint's OpenAI-style
//!   `/images/generations`, or an Ollama server's `/api/generate` for a build that still has it.
//!
//! The wire is curl, as for the chat runners, so a request stays off the Tokio reactor and a key
//! reaches curl through its environment, never its arguments.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use command::r#async::Command;
use krustui::{AppContext, SingletonEntity};
use serde_json::{json, Value};
use settings::Setting as _;

use crate::ai::api_providers::{agent_pool, ApiProviderKeys, ApiRoute, FailoverTarget};
use crate::ai::image_providers::{self, ImageModel, ImageProvider};
use crate::ai::model_caps;
use crate::settings::AISettings;

const DEFAULT_SIDE: u32 = 1024;
const MIN_SIDE: u32 = 256;
const MAX_SIDE: u32 = 2048;
/// The longest one request may take: a local model has to load and then draw.
const MAX_SECS: u64 = 1_200;
/// The environment variable curl reads a key from, and the header it expands from it.
const KEY_ENV: &str = "KRUST_API_KEY";
const AUTHORIZATION_HEADER: &str = "Authorization: Bearer {{KRUST_API_KEY}}";

/// What to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRequest {
    pub prompt: String,
    /// Where to save the PNG (an absolute path); the app's images folder when `None`.
    pub out: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub seed: Option<u64>,
    /// A picture to copy or edit.
    pub reference: Option<PathBuf>,
    /// Draw on a flat background, then cut it out.
    pub transparent: bool,
    /// Try only this backend: `command`, `images` or `pool`.
    pub backend: Option<String>,
}

impl ImageRequest {
    /// Reads the arguments of the `generate_image` tool.
    pub fn from_arguments(arguments: &Value) -> Result<Self, String> {
        let text = |key: &str| {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let prompt = text("prompt")
            .ok_or("generate_image needs a `prompt`.")?
            .to_string();
        let (width, height) = match text("size") {
            Some(size) => parse_size(size).ok_or_else(|| {
                format!("`size` is WIDTHxHEIGHT between {MIN_SIDE} and {MAX_SIDE}, e.g. 1024x1024, not {size:?}.")
            })?,
            None => (DEFAULT_SIDE, DEFAULT_SIDE),
        };
        Ok(Self {
            prompt,
            out: text("out").map(PathBuf::from),
            width,
            height,
            seed: arguments.get("seed").and_then(Value::as_u64),
            reference: text("reference_image").map(PathBuf::from),
            transparent: arguments
                .get("transparent")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            backend: text("backend").map(str::to_string),
        })
    }
}

/// `1024x768` as `(1024, 768)`, when both sides are in range.
pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (width, height) = text.split_once(['x', 'X'])?;
    let width: u32 = width.trim().parse().ok()?;
    let height: u32 = height.trim().parse().ok()?;
    let in_range = |side: u32| (MIN_SIDE..=MAX_SIDE).contains(&side);
    (in_range(width) && in_range(height)).then_some((width, height))
}

/// The picture that was made.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageResult {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub backend: String,
    pub seconds: f64,
}

impl ImageResult {
    /// What the tool and the command report: the path, and a markdown image for a transcript that
    /// shows them.
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path.to_string_lossy(),
            "width": self.width,
            "height": self.height,
            "backend": self.backend,
            "seconds": (self.seconds * 10.0).round() / 10.0,
            "markdown": format!("![]({})", self.path.display()),
        })
    }
}

/// A picture supplier's model with the key to use it (Settings → AI → Images).
#[derive(Clone)]
pub struct ImageTarget {
    pub provider: &'static ImageProvider,
    pub model: &'static ImageModel,
    pub key: String,
}

/// One way of making the picture, worked out on the main thread (it reads the settings and the
/// agent pool) and run off it.
#[derive(Clone)]
pub enum Attempt {
    /// The local command.
    Command(String),
    /// A model on the Images page's failover list.
    Image(ImageTarget),
    /// A model in the agent pool that makes images.
    Api(ApiRoute),
}

impl Attempt {
    fn label(&self) -> String {
        match self {
            Self::Command(program) => format!("local command {program}"),
            Self::Image(target) => format!("{} · {}", target.provider.display_name, target.model.display_name),
            Self::Api(route) => format!("{} · {}", route.provider_name, route.model_name),
        }
    }
}

/// The local command to try: the setting, else the environment, else the image-lab engine if it
/// is where it was installed.
fn command_path(app: &AppContext) -> Option<String> {
    let configured = AISettings::as_ref(app)
        .image_command
        .value()
        .clone()
        .filter(|command| !command.trim().is_empty());
    let from_env = std::env::var("KRUST_IMAGE_COMMAND")
        .ok()
        .filter(|command| !command.trim().is_empty());
    let default = std::env::var("HOME")
        .ok()
        .map(|home| format!("{home}/github/image-lab/bin/imagine-engine"))
        .filter(|path| Path::new(path).is_file());
    configured.or(from_env).or(default)
}

/// How to make a picture: the backends to try, in order, and where pictures go when the request
/// names no file.
pub struct Plan {
    pub attempts: Vec<Attempt>,
    pub images_dir: PathBuf,
}

/// Where pictures are saved by default, kept on the data drive when it can be: the setting
/// `image_output_dir`, else `KRUST_IMAGES_DIR`, else `~/github/image-lab/images` if that project
/// is there, else the app's own images folder.
fn images_dir(app: &AppContext) -> PathBuf {
    let configured = AISettings::as_ref(app)
        .image_output_dir
        .value()
        .clone()
        .filter(|dir| !dir.trim().is_empty());
    let from_env = std::env::var("KRUST_IMAGES_DIR")
        .ok()
        .filter(|dir| !dir.trim().is_empty());
    let lab = std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join("github/image-lab"))
        .filter(|lab| lab.is_dir())
        .map(|lab| lab.join("images"));
    configured
        .or(from_env)
        .map(PathBuf::from)
        .or(lab)
        .unwrap_or_else(|| krust_core::paths::state_dir().join("images"))
}

/// The backends to try for a request, in order. Runs on the main thread.
pub fn plan(backend: Option<&str>, app: &AppContext) -> Plan {
    let only = |name: &str| backend.is_none_or(|wanted| wanted.eq_ignore_ascii_case(name));
    let mut attempts = Vec::new();
    if only("command") {
        if let Some(command) = command_path(app) {
            attempts.push(Attempt::Command(command));
        }
    }
    if only("images") {
        let keys = ApiProviderKeys::as_ref(app);
        for id in image_providers::failover_rows(app) {
            if image_providers::is_disabled(&id, app) {
                continue;
            }
            let Some((provider, model)) = image_providers::parse_model_id(&id) else {
                continue;
            };
            if let Some(key) = keys.key_for(provider.id) {
                attempts.push(Attempt::Image(ImageTarget {
                    provider,
                    model,
                    key: key.to_string(),
                }));
            }
        }
    }
    if only("pool") {
        for target in agent_pool(app) {
            if let FailoverTarget::Api(route) = target {
                if model_caps::caps_of(&route.model_id, app).image_out {
                    attempts.push(Attempt::Api(route));
                }
            }
        }
    }
    Plan {
        attempts,
        images_dir: images_dir(app),
    }
}

pub const NO_BACKEND: &str = "no way to make a picture is set up: set `image_command` (a program \
    that takes imagine.py's flags) in Settings, or switch on a model that makes images \
    (a custom endpoint model with the `image_out` capability).";

/// Makes the picture on the first backend that can.
pub async fn run(plan: Plan, request: &ImageRequest) -> Result<ImageResult, String> {
    if plan.attempts.is_empty() {
        return Err(NO_BACKEND.to_string());
    }
    let out = output_path(request, &plan.images_dir);
    if !out.is_absolute() {
        return Err(format!("`out` must be an absolute path, not {}.", out.display()));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let mut problems = Vec::new();
    for attempt in plan.attempts {
        let started = Instant::now();
        let made = match &attempt {
            Attempt::Command(program) => run_command(program, request, &out).await,
            Attempt::Image(target) => run_image_target(target, request, &out).await,
            Attempt::Api(route) => run_api(route, request, &out).await,
        };
        match made {
            Ok(()) => {
                let (width, height) = png_size(&out).unwrap_or((request.width, request.height));
                return Ok(ImageResult {
                    path: out,
                    width,
                    height,
                    backend: attempt.label(),
                    seconds: started.elapsed().as_secs_f64(),
                });
            }
            Err(error) => problems.push(format!("{}: {error}", attempt.label())),
        }
    }
    Err(format!(
        "no backend could make the picture:\n  {}",
        problems.join("\n  ")
    ))
}

fn output_path(request: &ImageRequest, images_dir: &Path) -> PathBuf {
    if let Some(out) = &request.out {
        return out.clone();
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    images_dir.join(format!("imagine-{stamp}-{}.png", slug(&request.prompt)))
}

/// The first words of a prompt as a file name part.
pub fn slug(text: &str) -> String {
    let lowered: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let words: Vec<&str> = lowered.split('-').filter(|word| !word.is_empty()).collect();
    let joined: String = words.join("-").chars().take(40).collect();
    if joined.is_empty() {
        "image".to_string()
    } else {
        joined
    }
}

/// A PNG's width and height from its header.
pub fn png_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read as _;
    let mut header = [0u8; 24];
    std::fs::File::open(path).ok()?.read_exact(&mut header).ok()?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    Some((width, height))
}

// ---------------------------------------------------------------------------
// The local command
// ---------------------------------------------------------------------------

async fn run_command(program: &str, request: &ImageRequest, out: &Path) -> Result<(), String> {
    let mut args: Vec<String> = vec![
        "--json".into(),
        "--no-show".into(),
        "--out".into(),
        out.to_string_lossy().into_owned(),
        "--size".into(),
        format!("{}x{}", request.width, request.height),
    ];
    if let Some(seed) = request.seed {
        args.extend(["--seed".to_string(), seed.to_string()]);
    }
    if let Some(reference) = &request.reference {
        args.extend(["--ref".to_string(), reference.to_string_lossy().into_owned()]);
    }
    if request.transparent {
        args.push("--transparent".into());
    }
    args.extend(["--".to_string(), request.prompt.clone()]);
    let mut command = Command::new(program);
    command.args(args);
    let output = command
        .output()
        .await
        .map_err(|error| format!("could not run it: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    check_command_output(&stdout, out)
}

/// Reads the JSON line an engine prints: an `error` is a failure, otherwise the file must exist.
pub fn check_command_output(stdout: &str, out: &Path) -> Result<(), String> {
    let last = stdout.lines().rev().find(|line| !line.trim().is_empty());
    let answer: Option<Value> = last.and_then(|line| serde_json::from_str(line).ok());
    match answer {
        Some(answer) => {
            if let Some(error) = answer.get("error").and_then(Value::as_str) {
                return Err(error.to_string());
            }
            if out.is_file() {
                Ok(())
            } else {
                Err("it reported success but saved no file".to_string())
            }
        }
        None => Err(format!(
            "it printed no JSON line (got {:?})",
            stdout.chars().take(200).collect::<String>()
        )),
    }
}

// ---------------------------------------------------------------------------
// Models in the agent pool
// ---------------------------------------------------------------------------

async fn run_api(route: &ApiRoute, request: &ImageRequest, out: &Path) -> Result<(), String> {
    if request.reference.is_some() {
        return Err("it cannot take a reference image".to_string());
    }
    let dir = out.parent().unwrap_or(Path::new("/tmp"));
    let bytes = if route.is_local() {
        ollama_generate(route, request, dir).await?
    } else {
        openai_generate(route, request, out, dir).await?
    };
    if let Some(bytes) = bytes {
        std::fs::write(out, bytes).map_err(|error| format!("could not save the picture: {error}"))?;
    }
    Ok(())
}

/// An Ollama server's `/api/generate`, for a build that still has image generation (0.32.5 and
/// earlier). The picture comes back as base64 in `image`.
async fn ollama_generate(
    route: &ApiRoute,
    request: &ImageRequest,
    dir: &Path,
) -> Result<Option<Vec<u8>>, String> {
    let base = route.base_url.trim_end_matches("/v1");
    let mut body = json!({
        "model": route.model,
        "prompt": request.prompt,
        "stream": false,
        "width": request.width,
        "height": request.height,
    });
    if let Some(seed) = request.seed {
        body["seed"] = json!(seed);
    }
    let reply = curl_json(&format!("{base}/api/generate"), None, &body, dir).await?;
    let encoded = reply
        .get("image")
        .and_then(Value::as_str)
        .or_else(|| reply.pointer("/images/0").and_then(Value::as_str))
        .ok_or("the server answered with no image")?;
    decode_image(encoded).map(Some)
}

/// An OpenAI-style `/images/generations`. The picture comes back as base64 or, from some
/// providers, as a URL that is fetched straight to `out`.
async fn openai_generate(
    route: &ApiRoute,
    request: &ImageRequest,
    out: &Path,
    dir: &Path,
) -> Result<Option<Vec<u8>>, String> {
    let mut body = json!({
        "model": route.model,
        "prompt": request.prompt,
        "n": 1,
        "size": format!("{}x{}", request.width, request.height),
        "response_format": "b64_json",
    });
    if request.transparent {
        body["background"] = json!("transparent");
    }
    let key = (!route.api_key.is_empty()).then_some(route.api_key.as_str());
    let reply = curl_json(
        &format!("{}/images/generations", route.base_url),
        key,
        &body,
        dir,
    )
    .await?;
    picture_from_reply(&reply, out).await
}

/// A model on the Images page: the request is written the way its supplier wants it.
async fn run_image_target(target: &ImageTarget, request: &ImageRequest, out: &Path) -> Result<(), String> {
    if request.reference.is_some() && target.provider.wire != image_providers::Wire::OpenAi {
        return Err("it cannot take a reference image".to_string());
    }
    if request.transparent && target.provider.wire != image_providers::Wire::OpenAi {
        return Err("it cannot draw on a transparent background".to_string());
    }
    let dir = out.parent().unwrap_or(Path::new("/tmp"));
    let body = image_providers::request_body(
        target.provider,
        target.model.id,
        &request.prompt,
        request.width,
        request.height,
        request.transparent,
    );
    let reply = match &request.reference {
        // A reference picture goes to OpenAI's edits endpoint, as a form with the picture in it.
        Some(reference) => {
            let mut fields = vec![
                ("model", target.model.id.to_string()),
                ("prompt", request.prompt.clone()),
                ("size", format!("{}x{}", request.width, request.height)),
            ];
            if request.transparent {
                fields.push(("background", "transparent".to_string()));
            }
            curl_form(
                &format!("{}/images/edits", target.provider.base_url),
                Some(&target.key),
                &fields,
                "image",
                reference,
                dir,
            )
            .await?
        }
        None => {
            curl_json(
                &format!("{}/images/generations", target.provider.base_url),
                Some(&target.key),
                &body,
                dir,
            )
            .await?
        }
    };
    if let Some(bytes) = picture_from_reply(&reply, out).await? {
        std::fs::write(out, bytes).map_err(|error| format!("could not save the picture: {error}"))?;
    }
    Ok(())
}

/// The picture in an `/images/generations` answer: base64 data, or a URL fetched straight to
/// `out` (then `None`).
async fn picture_from_reply(reply: &Value, out: &Path) -> Result<Option<Vec<u8>>, String> {
    let first = reply.pointer("/data/0").ok_or("the answer has no `data`")?;
    if let Some(encoded) = first.get("b64_json").and_then(Value::as_str) {
        return decode_image(encoded).map(Some);
    }
    if let Some(url) = first.get("url").and_then(Value::as_str) {
        curl_download(url, out).await?;
        return Ok(None);
    }
    Err("the answer has neither `b64_json` nor `url`".to_string())
}

/// Base64 picture data, with or without a `data:` URL prefix.
pub fn decode_image(encoded: &str) -> Result<Vec<u8>, String> {
    let data = encoded.split_once("base64,").map_or(encoded, |(_, rest)| rest);
    base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|error| format!("the picture data is not valid base64: {error}"))
}

/// POSTs JSON with curl and returns the JSON answer. The body goes through a short-lived file in
/// `dir`, the key through the environment.
async fn curl_json(url: &str, key: Option<&str>, body: &Value, dir: &Path) -> Result<Value, String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let stamp = unique_stamp();
    let body_path = dir.join(format!(".request-{stamp}.json"));
    std::fs::write(&body_path, body.to_string()).map_err(|error| error.to_string())?;
    let content_args = vec![
        "--header".to_string(),
        "Content-Type: application/json".to_string(),
        "--data-binary".to_string(),
        format!("@{}", body_path.display()),
    ];
    let answer = curl_call(url, key, dir, &stamp, content_args).await;
    let _ = std::fs::remove_file(&body_path);
    answer
}

/// POSTs a multipart form (text fields and one file) with curl and returns the JSON answer.
/// `--form-string` keeps a prompt that starts with `@` or `<` from being read as a file.
async fn curl_form(
    url: &str,
    key: Option<&str>,
    fields: &[(&str, String)],
    file_field: &str,
    file: &Path,
    dir: &Path,
) -> Result<Value, String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let mut content_args: Vec<String> = Vec::new();
    for (name, value) in fields {
        content_args.push("--form-string".to_string());
        content_args.push(format!("{name}={value}"));
    }
    content_args.push("--form".to_string());
    content_args.push(format!("{file_field}=@\"{}\"", file.display()));
    curl_call(url, key, dir, &unique_stamp(), content_args).await
}

fn unique_stamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0)
        .to_string()
}

/// Runs one curl POST whose body arguments are `content_args`, and reads the JSON answer; the key
/// goes through the environment.
async fn curl_call(
    url: &str,
    key: Option<&str>,
    dir: &Path,
    stamp: &str,
    content_args: Vec<String>,
) -> Result<Value, String> {
    let reply_path = dir.join(format!(".reply-{stamp}.json"));
    let mut args: Vec<String> = vec![
        "--silent".into(),
        "--show-error".into(),
        "--max-time".into(),
        MAX_SECS.to_string(),
    ];
    if key.is_some() {
        args.extend([
            "--variable".to_string(),
            format!("%{KEY_ENV}"),
            "--expand-header".to_string(),
            AUTHORIZATION_HEADER.to_string(),
        ]);
    }
    args.extend(content_args);
    args.extend([
        "--output".to_string(),
        reply_path.to_string_lossy().into_owned(),
        "--write-out".to_string(),
        "%{http_code}".to_string(),
        url.to_string(),
    ]);
    let mut command = Command::new("/usr/bin/curl");
    command.args(args);
    if let Some(key) = key {
        command.env(KEY_ENV, key);
    }
    let output = command.output().await;
    let reply = std::fs::read(&reply_path);
    let _ = std::fs::remove_file(&reply_path);
    let output = output.map_err(|error| format!("could not run curl: {error}"))?;
    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let reply = reply.unwrap_or_default();
    if !output.status.success() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let parsed: Option<Value> = serde_json::from_slice(&reply).ok();
    if !status.starts_with('2') {
        let detail = parsed
            .as_ref()
            .and_then(|value| value.pointer("/error/message").or_else(|| value.get("error")))
            .map(|error| error.as_str().map_or_else(|| error.to_string(), str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&reply).chars().take(200).collect());
        return Err(format!("answered {status}: {detail}"));
    }
    parsed.ok_or_else(|| "the answer is not JSON".to_string())
}

async fn curl_download(url: &str, out: &Path) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/curl");
    command.args([
        "--silent".to_string(),
        "--show-error".to_string(),
        "--fail".to_string(),
        "--location".to_string(),
        "--max-time".to_string(),
        MAX_SECS.to_string(),
        "--output".to_string(),
        out.to_string_lossy().into_owned(),
        url.to_string(),
    ]);
    let output = command
        .output()
        .await
        .map_err(|error| format!("could not run curl: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not fetch the picture: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

// ---------------------------------------------------------------------------
// Drawing a picture in the terminal (Kitty graphics protocol)
// ---------------------------------------------------------------------------

/// The escape sequences that draw a PNG at the cursor, in chunks of at most 4096 base64 bytes,
/// `columns` terminal cells wide.
pub fn kitty_sequence(png: &[u8], columns: u32) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let chunks: Vec<&str> = encoded
        .as_bytes()
        .chunks(4096)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect();
    let mut sequence = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        let control = if index == 0 {
            format!("a=T,f=100,t=d,c={columns},m={more}")
        } else {
            format!("m={more}")
        };
        sequence.push_str(&format!("\x1b_G{control};{chunk}\x1b\\"));
    }
    sequence
}

// ---------------------------------------------------------------------------
// The `imagine` command (a hidden worker mode of the krust binary)
// ---------------------------------------------------------------------------

/// What the command line gave `imagine`.
pub struct CliOptions {
    pub prompt: String,
    pub out: Option<PathBuf>,
    pub size: Option<String>,
    pub seed: Option<u64>,
    pub reference: Option<PathBuf>,
    pub transparent: bool,
    pub backend: Option<String>,
    pub json: bool,
    pub show: bool,
    pub no_show: bool,
    pub columns: u32,
}

/// Makes the picture through the running app (which owns the settings, the keys and the agent
/// pool), then prints its path, or its JSON, and draws it when the terminal can.
pub fn run_cli(options: CliOptions) -> anyhow::Result<()> {
    let absolute = |path: PathBuf| -> anyhow::Result<PathBuf> {
        Ok(if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        })
    };
    let default_name = format!(
        "imagine-{}-{}.png",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0),
        slug(&options.prompt)
    );
    let mut arguments = json!({ "prompt": options.prompt });
    let out = absolute(options.out.unwrap_or_else(|| PathBuf::from(default_name)))?;
    arguments["out"] = json!(out.to_string_lossy());
    if let Some(size) = options.size {
        arguments["size"] = json!(size);
    }
    if let Some(seed) = options.seed {
        arguments["seed"] = json!(seed);
    }
    if let Some(reference) = options.reference {
        arguments["reference_image"] = json!(absolute(reference)?.to_string_lossy());
    }
    if options.transparent {
        arguments["transparent"] = json!(true);
    }
    if let Some(backend) = options.backend {
        arguments["backend"] = json!(backend);
    }

    let reply = crate::ai::queue_tools::socket_request("image", &arguments)
        .map_err(|error| anyhow::anyhow!("imagine: {error}"))?;
    if options.json {
        println!("{reply}");
        return Ok(());
    }
    let answer: Value = serde_json::from_str(&reply)?;
    let path = answer
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("imagine: the app answered with no path: {reply}"))?;
    let draw = options.show
        || (!options.no_show
            && std::io::stdout().is_terminal()
            && crate::ai::agent_sdk::is_running_in_krust());
    if draw {
        use std::io::Write as _;
        let png = std::fs::read(path)?;
        let mut stdout = std::io::stdout();
        writeln!(stdout, "{}", kitty_sequence(&png, options.columns))?;
        stdout.flush()?;
    }
    println!("{path}");
    Ok(())
}

#[cfg(test)]
#[path = "image_gen_tests.rs"]
mod tests;
