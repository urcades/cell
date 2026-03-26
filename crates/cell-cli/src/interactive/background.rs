use super::*;

#[derive(Debug)]
pub struct ActivePrompt {
    pub event_rx: mpsc::Receiver<AgentEvent>,
    pub result_rx: mpsc::Receiver<Result<PromptRun, String>>,
    pub handle: Option<thread::JoinHandle<()>>,
    pub aborted: bool,
    #[allow(dead_code)]
    pub started_at: Instant,
    pub completion_result: Option<Result<PromptRun, String>>,
    pub linger_after_completion: bool,
}

#[derive(Debug)]
pub struct ActiveShare {
    pub result_rx: mpsc::Receiver<Result<ShareTaskResult, String>>,
    pub cancel_flag: Arc<AtomicBool>,
    pub handle: Option<thread::JoinHandle<()>>,
    #[allow(dead_code)]
    pub started_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareTaskResult {
    Success {
        viewer_url: String,
        gist_url: String,
    },
    Cancelled,
}

#[derive(Debug)]
pub struct ActiveAuthFlow {
    pub provider: String,
    pub ui_rx: mpsc::Receiver<AuthUiRequest>,
    pub response_tx: mpsc::Sender<AuthUiResponse>,
    pub result_rx: mpsc::Receiver<Result<OAuthCredentials, String>>,
    pub cancel_flag: Arc<AtomicBool>,
    pub handle: thread::JoinHandle<()>,
    pub started_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthPromptKind {
    Prompt,
    ManualCode,
}

#[derive(Clone, Debug)]
pub enum AuthUiRequest {
    ShowAuth(OAuthAuthInfo),
    Prompt {
        prompt: OAuthPrompt,
        kind: AuthPromptKind,
    },
    Progress(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthUiResponse {
    Input(String),
    Cancelled,
}

pub struct ChannelOAuthLoginBridge {
    pub ui_tx: mpsc::Sender<AuthUiRequest>,
    pub response_tx: mpsc::Sender<AuthUiResponse>,
    pub response_rx: Arc<Mutex<mpsc::Receiver<AuthUiResponse>>>,
    pub cancel_flag: Arc<AtomicBool>,
}

impl OAuthLoginBridge for ChannelOAuthLoginBridge {
    fn show_auth(&self, info: OAuthAuthInfo) -> Result<(), String> {
        self.ui_tx
            .send(AuthUiRequest::ShowAuth(info))
            .map_err(|_| "Failed to send auth URL to interactive UI.".to_string())
    }

    fn prompt(&self, prompt: OAuthPrompt) -> Result<String, String> {
        self.ui_tx
            .send(AuthUiRequest::Prompt {
                prompt,
                kind: AuthPromptKind::Prompt,
            })
            .map_err(|_| "Failed to request login input.".to_string())?;
        match self
            .response_rx
            .lock()
            .map_err(|_| "Failed to lock login input receiver.".to_string())?
            .recv()
            .map_err(|_| "Login input channel disconnected.".to_string())?
        {
            AuthUiResponse::Input(value) => Ok(value),
            AuthUiResponse::Cancelled => Err("Login cancelled".to_string()),
        }
    }

    fn manual_code_input(&self, prompt: OAuthPrompt) -> Result<String, String> {
        self.ui_tx
            .send(AuthUiRequest::Prompt {
                prompt,
                kind: AuthPromptKind::ManualCode,
            })
            .map_err(|_| "Failed to request authorization code input.".to_string())?;
        match self
            .response_rx
            .lock()
            .map_err(|_| "Failed to lock login input receiver.".to_string())?
            .recv()
            .map_err(|_| "Login input channel disconnected.".to_string())?
        {
            AuthUiResponse::Input(value) => Ok(value),
            AuthUiResponse::Cancelled => Err("Login cancelled".to_string()),
        }
    }

    fn progress(&self, message: &str) -> Result<(), String> {
        self.ui_tx
            .send(AuthUiRequest::Progress(message.to_string()))
            .map_err(|_| "Failed to send login progress to interactive UI.".to_string())
    }

    fn cancel_pending_input(&self) {
        let _ = self.response_tx.send(AuthUiResponse::Cancelled);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::SeqCst)
    }
}

pub fn ensure_github_cli_ready() -> Result<(), String> {
    let auth = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                "GitHub CLI (gh) is not installed. Install it from https://cli.github.com/"
                    .to_string()
            }
            _ => format!("Failed to invoke gh auth status: {error}"),
        })?;
    if auth.status.success() {
        Ok(())
    } else {
        Err("GitHub CLI is not logged in. Run 'gh auth login' first.".to_string())
    }
}

pub fn run_share_task(
    exported_path: PathBuf,
    cancel_flag: Arc<AtomicBool>,
) -> Result<ShareTaskResult, String> {
    let result = run_share_task_inner(&exported_path, &cancel_flag);
    let _ = fs::remove_file(&exported_path);
    result
}

pub fn run_share_task_inner(
    exported_path: &Path,
    cancel_flag: &AtomicBool,
) -> Result<ShareTaskResult, String> {
    if cancel_flag.load(Ordering::SeqCst) {
        return Ok(ShareTaskResult::Cancelled);
    }

    let mut gist = Command::new("gh")
        .args(["gist", "create", "--public=false"])
        .arg(exported_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Failed to create gist: {error}"))?;

    loop {
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = gist.kill();
            let _ = gist.wait();
            return Ok(ShareTaskResult::Cancelled);
        }

        match gist.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                if let Some(mut stream) = gist.stdout.take() {
                    let _ = stream.read_to_string(&mut stdout);
                }
                let mut stderr = String::new();
                if let Some(mut stream) = gist.stderr.take() {
                    let _ = stream.read_to_string(&mut stderr);
                }
                if !status.success() {
                    let stderr = stderr.trim().to_string();
                    return Err(if stderr.is_empty() {
                        "Failed to create gist.".to_string()
                    } else {
                        format!("Failed to create gist: {stderr}")
                    });
                }

                let gist_url = stdout.trim().to_string();
                let gist_id = gist_url
                    .split('/')
                    .next_back()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| "Failed to parse gist ID from gh output".to_string())?;
                let viewer_url = cell_config::get_share_viewer_url(gist_id);
                return Ok(ShareTaskResult::Success {
                    viewer_url,
                    gist_url,
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(75)),
            Err(error) => return Err(format!("Failed to create gist: {error}")),
        }
    }
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return write_clipboard_command("pbcopy", &[], text);
    }
    #[cfg(target_os = "windows")]
    {
        return write_clipboard_command("clip", &[], text);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        for (program, args) in [
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ] {
            if write_clipboard_command(program, &args, text).is_ok() {
                return Ok(());
            }
        }
        Err("No supported clipboard command found (tried wl-copy, xclip, xsel).".to_string())
    }
}

pub fn paste_clipboard_image_to_temp_file() -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "macos")]
    {
        return paste_macos_clipboard_image();
    }
    #[cfg(target_os = "linux")]
    {
        return paste_linux_clipboard_image();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
pub fn paste_macos_clipboard_image() -> Result<Option<PathBuf>, String> {
    let path = temp_clipboard_image_path("png");
    let script = r#"
on run argv
    set outPath to item 1 of argv
    try
        set imageData to the clipboard as «class PNGf»
    on error
        return "NO_IMAGE"
    end try
    set fileRef to open for access POSIX file outPath with write permission
    try
        set eof fileRef to 0
        write imageData to fileRef
        close access fileRef
    on error errMsg
        try
            close access fileRef
        end try
        error errMsg
    end try
    return "OK"
end run
"#;
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg("--")
        .arg(&path)
        .output();
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    if String::from_utf8_lossy(&output.stdout).trim() == "NO_IMAGE" {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(path))
}

#[cfg(target_os = "linux")]
pub fn paste_linux_clipboard_image() -> Result<Option<PathBuf>, String> {
    if let Some((bytes, mime_type)) = read_linux_clipboard_image() {
        let extension = image_extension_for_mime_type(mime_type).unwrap_or("png");
        let path = temp_clipboard_image_path(extension);
        fs::write(&path, bytes).map_err(|error| error.to_string())?;
        return Ok(Some(path));
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
pub fn read_linux_clipboard_image() -> Option<(Vec<u8>, &'static str)> {
    let preferred_types = ["image/png", "image/jpeg", "image/webp", "image/gif"];
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .ok()
            .is_some_and(|value| value == "wayland");
    if is_wayland {
        let list = Command::new("wl-paste")
            .args(["--list-types"])
            .output()
            .ok()?;
        if !list.status.success() {
            return None;
        }
        let available = String::from_utf8_lossy(&list.stdout);
        let mime_type = preferred_types
            .iter()
            .find(|candidate| available.lines().any(|line| line.trim() == **candidate))?;
        let data = Command::new("wl-paste")
            .args(["--type", mime_type, "--no-newline"])
            .output()
            .ok()?;
        if data.status.success() && !data.stdout.is_empty() {
            return Some((data.stdout, mime_type));
        }
    }

    let targets = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
        .output()
        .ok();
    let target_text = targets
        .as_ref()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    for mime_type in preferred_types {
        if !target_text.is_empty() && !target_text.lines().any(|line| line.trim() == mime_type) {
            continue;
        }
        let data = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", mime_type, "-o"])
            .output()
            .ok()?;
        if data.status.success() && !data.stdout.is_empty() {
            return Some((data.stdout, mime_type));
        }
    }
    None
}

#[cfg(target_os = "linux")]
pub fn image_extension_for_mime_type(mime_type: &str) -> Option<&'static str> {
    match mime_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or(mime_type)
    {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

pub fn temp_clipboard_image_path(extension: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "pi-clipboard-{}-{}.{}",
        std::process::id(),
        millis,
        extension
    ))
}

pub fn write_clipboard_command(program: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("{program} is not installed."),
            _ => format!("Failed to start {program}: {error}"),
        })?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| format!("Failed to write to {program}: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("Failed to wait for {program}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("{program} exited with {}", output.status)
        } else {
            stderr
        })
    }
}
