//! Aionix Connect Dev Launcher.
//!
//! One window that prepares a local validation run:
//!
//! 1. finds the repository and the built applications;
//! 2. starts the server stack — Docker Compose staging (`infra/local/staging.ps1`, the
//!    supported path) or, when Docker is not usable, the same server binaries as native
//!    processes (`infra/local/native-stack.ps1`, development only);
//! 3. waits for readiness;
//! 4. provisions test data through `aionix-admin` (organization, technician account,
//!    enrollment token) — credentials are generated and kept in `.data/dev/`;
//! 5. opens Aionix Connect (host) and Aionix Connect Técnico.
//!
//! Every step shows its state; the output of the scripts is kept in `.data/dev/logs/`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("aionix-launcher runs on Windows only");
}

#[cfg(windows)]
fn main() {
    app::main();
}

#[cfg(windows)]
mod app {
    use std::{
        collections::VecDeque,
        path::{Path, PathBuf},
        process::Stdio,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use aionix_client::ApiClient;
    use aionix_control_api::ErrorCode;
    use aionix_desktop::{icon, theme};
    use clap::Parser;
    use egui::{Align, Layout, RichText, Stroke};
    use serde::{Deserialize, Serialize};
    use tokio::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// Services started by the scripts must not receive the launcher's console signals.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    #[derive(Parser, Debug, Clone)]
    #[command(name = "aionix-launcher", about = "Aionix Connect Dev Launcher")]
    struct Args {
        /// Repository root (default: found by walking up from the executable).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Force a server mode instead of detecting it.
        #[arg(long, value_enum)]
        mode: Option<Mode>,
        /// Start everything as soon as the window opens.
        #[arg(long)]
        auto_start: bool,
        /// Prepare an internet test (temporary tunnel + package for another computer).
        #[arg(long)]
        internet: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
    enum Mode {
        Docker,
        Native,
    }

    impl Mode {
        fn label(self) -> &'static str {
            match self {
                Mode::Docker => "Docker (staging)",
                Mode::Native => "Nativo (desenvolvimento)",
            }
        }
        fn server(self) -> &'static str {
            match self {
                Mode::Docker => "http://127.0.0.1:8080",
                Mode::Native => "http://127.0.0.1:18080",
            }
        }
        fn signal(self) -> Option<&'static str> {
            match self {
                Mode::Docker => None,
                Mode::Native => Some("ws://127.0.0.1:18081/v1/signal"),
            }
        }
        fn slug(self) -> &'static str {
            match self {
                Mode::Docker => "docker",
                Mode::Native => "native",
            }
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    enum StepState {
        Pending,
        Running,
        Done(String),
        Failed(String),
    }

    struct Step {
        title: &'static str,
        state: StepState,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct Provision {
        org_id: String,
        email: String,
        password: String,
        #[serde(default)]
        server_key: String,
    }

    struct State {
        steps: Vec<Step>,
        log: VecDeque<String>,
        running: bool,
        mode: Option<Mode>,
        docker_available: Option<bool>,
        creds: Option<Provision>,
        started_at: Option<Instant>,
        /// Test with a computer on another network through a temporary public tunnel.
        internet: bool,
        public_url: Option<String>,
    }

    type Shared = Arc<Mutex<State>>;

    struct Paths {
        repo: PathBuf,
        bin: PathBuf,
        dev: PathBuf,
    }

    const STEP_TITLES: [&str; 7] = [
        "Verificar ambiente",
        "Iniciar servidor",
        "Aguardar prontidão dos serviços",
        "Preparar dados de teste",
        "Acesso pela internet (túnel temporário)",
        "Abrir Aionix Connect (computador atendido)",
        "Abrir Aionix Connect Técnico",
    ];

    pub fn main() {
        let args = Args::parse();
        aionix_platform::windows::init_dpi_awareness();
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let repo = args.repo.clone().or_else(|| find_repo(&exe_dir));
        let state = Arc::new(Mutex::new(State {
            steps: STEP_TITLES
                .iter()
                .map(|t| Step {
                    title: t,
                    state: StepState::Pending,
                })
                .collect(),
            log: VecDeque::new(),
            running: false,
            mode: if args.internet {
                Some(Mode::Native)
            } else {
                args.mode
            },
            docker_available: None,
            creds: None,
            started_at: None,
            internet: args.internet,
            public_url: None,
        }));
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let paths = repo.map(|repo| Paths {
            dev: repo.join(".data").join("dev"),
            bin: exe_dir.clone(),
            repo,
        });
        if let Some(p) = &paths {
            let _ = std::fs::create_dir_all(p.dev.join("logs"));
            // Credentials of a previous run (shown before starting).
            for m in [Mode::Docker, Mode::Native] {
                if state.lock().unwrap().creds.is_none()
                    && let Some(c) = load_provision(&p.dev, m)
                {
                    state.lock().unwrap().creds = Some(c);
                }
            }
        }
        let paths = paths.map(Arc::new);
        {
            let s = state.clone();
            rt.spawn(async move {
                let ok = docker_ready().await;
                let mut st = s.lock().unwrap();
                st.docker_available = Some(ok);
                if st.mode.is_none() {
                    st.mode = Some(if ok { Mode::Docker } else { Mode::Native });
                }
            });
        }
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Aionix Connect Dev Launcher")
                .with_inner_size([820.0, 720.0])
                .with_min_inner_size([700.0, 600.0])
                .with_icon(icon::brand_icon(64, [0x4f, 0x7c, 0xff])),
            ..Default::default()
        };
        let auto = args.auto_start;
        let _ = eframe::run_native(
            "aionix-launcher",
            options,
            Box::new(move |cc| {
                theme::install(&cc.egui_ctx);
                Ok(Box::new(Launcher {
                    state,
                    rt,
                    paths,
                    auto,
                    show_password: false,
                }))
            }),
        );
    }

    fn find_repo(start: &Path) -> Option<PathBuf> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            if d.join("Cargo.toml").is_file()
                && d.join("infra").join("local").join("staging.ps1").is_file()
            {
                return Some(d.to_path_buf());
            }
            // dist/<name>/ next to the repository root
            dir = d.parent();
        }
        None
    }

    fn docker_exe() -> PathBuf {
        let bundled = PathBuf::from(r"C:\Program Files\Docker\Docker\resources\bin\docker.exe");
        if bundled.is_file() {
            bundled
        } else {
            PathBuf::from("docker")
        }
    }

    fn cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(program);
        c.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null());
        c
    }

    async fn docker_ready() -> bool {
        let out = cmd(docker_exe())
            .args(["info", "--format", "{{.ServerVersion}}"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        matches!(tokio::time::timeout(Duration::from_secs(15), out).await, Ok(Ok(s)) if s.success())
    }

    fn load_provision(dev: &Path, mode: Mode) -> Option<Provision> {
        let text =
            std::fs::read_to_string(dev.join(format!("{}-provision.json", mode.slug()))).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn save_provision(dev: &Path, mode: Mode, p: &Provision) -> Result<(), String> {
        let json = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
        std::fs::write(dev.join(format!("{}-provision.json", mode.slug())), json)
            .map_err(|e| e.to_string())
    }

    fn random_password() -> String {
        let alphabet = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
        let mut s = String::from("Aionix-");
        for _ in 0..14 {
            s.push(alphabet[rand_index(alphabet.len())] as char);
        }
        s
    }

    fn rand_index(n: usize) -> usize {
        let mut b = [0u8; 8];
        getrandom(&mut b);
        (u64::from_le_bytes(b) % n as u64) as usize
    }

    fn getrandom(buf: &mut [u8]) {
        // UUID v4 uses the OS CSPRNG.
        for chunk in buf.chunks_mut(16) {
            let u = uuid::Uuid::new_v4();
            chunk.copy_from_slice(&u.as_bytes()[..chunk.len()]);
        }
    }

    // ------------------------------------------------------------------ pipeline

    fn log(s: &Shared, line: impl Into<String>) {
        let mut st = s.lock().unwrap();
        st.log.push_back(line.into());
        while st.log.len() > 400 {
            st.log.pop_front();
        }
    }

    fn set_step(s: &Shared, i: usize, state: StepState) {
        s.lock().unwrap().steps[i].state = state;
    }

    /// Runs a script with its output going to a log file (services started by the script
    /// inherit that file, not a pipe we would wait on), mirroring new lines to the UI.
    async fn run_logged(
        s: &Shared,
        mut command: Command,
        log_file: &Path,
        timeout: Duration,
    ) -> Result<(), String> {
        let file = std::fs::File::create(log_file).map_err(|e| e.to_string())?;
        let err = file.try_clone().map_err(|e| e.to_string())?;
        command.stdout(Stdio::from(file)).stderr(Stdio::from(err));
        let mut child = command
            .spawn()
            .map_err(|e| format!("não foi possível executar: {e}"))?;
        let deadline = Instant::now() + timeout;
        let mut shown = 0usize;
        loop {
            let done = child.try_wait().map_err(|e| e.to_string())?;
            if let Ok(text) = std::fs::read(log_file) {
                let text = String::from_utf8_lossy(&text);
                let lines: Vec<&str> = text.lines().collect();
                for l in lines.iter().skip(shown) {
                    let l = l.trim_end();
                    if !l.is_empty() {
                        log(s, format!("  {l}"));
                    }
                }
                shown = lines.len();
            }
            if let Some(status) = done {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "terminou com código {:?} (veja {})",
                        status.code(),
                        log_file.display()
                    ))
                };
            }
            if Instant::now() > deadline {
                let _ = child.kill().await;
                return Err(format!("tempo esgotado (veja {})", log_file.display()));
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    }

    fn pwsh_script(repo: &Path, script: &str, args: &[&str]) -> Command {
        let shell = if which("pwsh") { "pwsh" } else { "powershell" };
        // Redirected output would otherwise use the OEM code page and mangle accents.
        let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
        let mut command = format!(
            "[Console]::OutputEncoding = [Text.Encoding]::UTF8; & {}",
            quote(&repo.join(script).to_string_lossy())
        );
        for a in args {
            command.push(' ');
            // Switches stay bare; values are quoted literals.
            if a.starts_with('-') && !a.contains(' ') {
                command.push_str(a);
            } else {
                command.push_str(&quote(a));
            }
        }
        command.push_str("; exit $LASTEXITCODE");
        let mut c = cmd(shell);
        c.arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(command)
            .current_dir(repo);
        c
    }

    fn which(name: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|p| {
            std::env::split_paths(&p).any(|d| d.join(format!("{name}.exe")).is_file())
        })
    }

    async fn http_ok(url: &str) -> bool {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .build();
        match client {
            Ok(c) => c
                .get(url)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success()),
            Err(_) => false,
        }
    }

    async fn ensure_docker(s: &Shared) -> Result<(), String> {
        if docker_ready().await {
            return Ok(());
        }
        let desktop = PathBuf::from(r"C:\Program Files\Docker\Docker\Docker Desktop.exe");
        if !desktop.is_file() {
            return Err("Docker Desktop não está instalado".into());
        }
        log(
            s,
            "Iniciando o Docker Desktop (na primeira vez ele pede para aceitar os termos de uso)...",
        );
        std::process::Command::new(&desktop)
            .spawn()
            .map_err(|e| format!("não foi possível abrir o Docker Desktop: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(300);
        while Instant::now() < deadline {
            if docker_ready().await {
                log(s, "Docker pronto.");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        Err("o Docker Desktop não ficou pronto em 5 minutos".into())
    }

    async fn admin(
        paths: &Paths,
        mode: Mode,
        args: &[&str],
        secret_env: Option<(&str, &str)>,
    ) -> Result<String, String> {
        let mut c = match mode {
            Mode::Native => {
                let mut c = cmd(paths.bin.join("aionix-admin.exe"));
                c.env(
                    "AIONIX_DATABASE_URL",
                    "postgres://aionix:aionix-dev@127.0.0.1:55432/aionix",
                );
                c.args(args);
                c
            }
            Mode::Docker => {
                let mut c = cmd(docker_exe());
                c.arg("compose")
                    .arg("-f")
                    .arg(paths.repo.join("infra/compose/docker-compose.yml"))
                    .arg("--env-file")
                    .arg(paths.repo.join("infra/local/.staging/config/.env"))
                    .args(["--profile", "tools", "run", "--rm", "-T"]);
                if let Some((k, _)) = secret_env {
                    c.args(["-e", k]);
                }
                c.args(["aionix-admin", "aionix-admin"]).args(args);
                c
            }
        };
        if let Some((k, v)) = secret_env {
            c.env(k, v);
        }
        let out = tokio::time::timeout(Duration::from_secs(180), c.output())
            .await
            .map_err(|_| "aionix-admin: tempo esgotado".to_owned())?
            .map_err(|e| format!("aionix-admin: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if out.status.success() {
            Ok(stdout)
        } else {
            Err(format!(
                "aionix-admin {}: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    async fn pipeline(s: Shared, paths: Arc<Paths>, mode: Mode) -> Result<(), String> {
        // 1. Environment
        set_step(&s, 0, StepState::Running);
        for exe in ["aionix-host.exe", "aionix-technician.exe"] {
            if !paths.bin.join(exe).is_file() {
                return fail(
                    &s,
                    0,
                    format!("{exe} não encontrado em {}", paths.bin.display()),
                );
            }
        }
        if mode == Mode::Native {
            for exe in [
                "aionix-api.exe",
                "aionix-signaling.exe",
                "aionix-relay.exe",
                "aionix-admin.exe",
            ] {
                if !paths.bin.join(exe).is_file() {
                    return fail(
                        &s,
                        0,
                        format!("{exe} não encontrado em {}", paths.bin.display()),
                    );
                }
            }
        }
        set_step(
            &s,
            0,
            StepState::Done(format!("Repositório: {}", paths.repo.display())),
        );

        // 2. Server
        set_step(&s, 1, StepState::Running);
        let logs = paths.dev.join("logs");
        let started = match mode {
            Mode::Docker => {
                if let Err(e) = ensure_docker(&s).await {
                    return fail(&s, 1, e);
                }
                log(
                    &s,
                    "Subindo a pilha Docker (a primeira vez compila a imagem do servidor: alguns minutos)...",
                );
                run_logged(
                    &s,
                    pwsh_script(&paths.repo, "infra/local/staging.ps1", &["-Action", "Up"]),
                    &logs.join("staging-up.log"),
                    Duration::from_secs(45 * 60),
                )
                .await
            }
            Mode::Native => {
                let bin = paths.bin.to_string_lossy().into_owned();
                let mut c = pwsh_script(
                    &paths.repo,
                    "infra/local/native-stack.ps1",
                    &["-Action", "Up", "-BinDir", &bin],
                );
                // The services must outlive this launcher.
                c.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
                run_logged(
                    &s,
                    c,
                    &logs.join("native-up.log"),
                    Duration::from_secs(5 * 60),
                )
                .await
            }
        };
        if let Err(e) = started {
            return fail(&s, 1, e);
        }
        set_step(&s, 1, StepState::Done(mode.label().into()));

        // 3. Readiness
        set_step(&s, 2, StepState::Running);
        let probes: Vec<&str> = match mode {
            Mode::Docker => vec![
                "http://127.0.0.1:8080/healthz",
                "http://127.0.0.1:8080/v1/server/info",
            ],
            Mode::Native => vec![
                "http://127.0.0.1:18080/readyz",
                "http://127.0.0.1:18081/readyz",
                "http://127.0.0.1:19090/readyz",
            ],
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let mut all = true;
            for p in &probes {
                all &= http_ok(p).await;
            }
            if all {
                break;
            }
            if Instant::now() > deadline {
                return fail(&s, 2, "os serviços não ficaram prontos em 2 minutos".into());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let api = match ApiClient::new(mode.server()) {
            Ok(a) => a,
            Err(e) => return fail(&s, 2, e.to_string()),
        };
        let info = match api.server_info().await {
            Ok(i) => i,
            Err(e) => return fail(&s, 2, format!("informações do servidor: {e}")),
        };
        let server_key = info
            .token_keys
            .first()
            .map(|k| k.to_base64())
            .unwrap_or_default();
        set_step(
            &s,
            2,
            StepState::Done(format!("{} · versão {}", info.name, info.version)),
        );

        // 4. Test data
        set_step(&s, 3, StepState::Running);
        let mut prov = load_provision(&paths.dev, mode);
        let valid = match &prov {
            Some(p) => match api.login(&p.email, &p.password).await {
                Ok(t) => {
                    let _ = api.logout(&t.refresh_token).await;
                    true
                }
                Err(e) if e.code() == Some(ErrorCode::InvalidCredentials) => false,
                Err(e) => return fail(&s, 3, format!("login de teste falhou: {e}")),
            },
            None => false,
        };
        if !valid {
            log(
                &s,
                "Criando organização, conta de técnico e permissões de teste...",
            );
            let org =
                match admin(&paths, mode, &["create-org", "--name", "Aionix Demo"], None).await {
                    Ok(o) => o.lines().last().unwrap_or_default().trim().to_owned(),
                    Err(e) => return fail(&s, 3, e),
                };
            let email = format!(
                "tecnico+{}@aionix.local",
                &uuid::Uuid::new_v4().simple().to_string()[..6]
            );
            let password = random_password();
            if let Err(e) = admin(
                &paths,
                mode,
                &["create-user", "--email", &email, "--name", "Técnico Demo"],
                Some(("AIONIX_NEW_USER_PASSWORD", &password)),
            )
            .await
            {
                return fail(&s, 3, e);
            }
            if let Err(e) = admin(
                &paths,
                mode,
                &[
                    "add-member",
                    "--org",
                    &org,
                    "--email",
                    &email,
                    "--role",
                    "technician",
                ],
                None,
            )
            .await
            {
                return fail(&s, 3, e);
            }
            let p = Provision {
                org_id: org,
                email,
                password,
                server_key: server_key.clone(),
            };
            if let Err(e) = save_provision(&paths.dev, mode, &p) {
                return fail(&s, 3, e);
            }
            prov = Some(p);
        }
        let prov = prov.expect("provisioned");
        s.lock().unwrap().creds = Some(prov.clone());
        // A fresh single-purpose enrollment token for this run (only used if the host is
        // not enrolled on this server yet).
        let token = match admin(
            &paths,
            mode,
            &[
                "enrollment-token",
                "--org",
                &prov.org_id,
                "--hours",
                "24",
                "--uses",
                "1",
            ],
            None,
        )
        .await
        {
            Ok(t) => t.lines().next().unwrap_or_default().trim().to_owned(),
            Err(e) => return fail(&s, 3, e),
        };
        set_step(&s, 3, StepState::Done(format!("Técnico: {}", prov.email)));

        // 5. Internet access (native mode): tunnel + package for the other computer.
        let internet = s.lock().unwrap().internet;
        let mut server = mode.server().to_owned();
        let mut signal = mode.signal().map(str::to_owned);
        if internet {
            set_step(&s, 4, StepState::Running);
            if mode != Mode::Native {
                return fail(
                    &s,
                    4,
                    "o teste pela internet usa o modo Nativo nesta versão".into(),
                );
            }
            log(
                &s,
                "Abrindo um túnel HTTPS temporário (Cloudflare) para o servidor local...",
            );
            let bin = paths.bin.to_string_lossy().into_owned();
            let mut c = pwsh_script(
                &paths.repo,
                "infra/local/public-tunnel.ps1",
                &["-Action", "Up", "-BinDir", &bin],
            );
            c.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
            if let Err(e) = run_logged(
                &s,
                c,
                &logs.join("public-tunnel.log"),
                Duration::from_secs(6 * 60),
            )
            .await
            {
                return fail(&s, 4, e);
            }
            let url = std::fs::read_to_string(paths.dev.join("public-url.txt"))
                .unwrap_or_default()
                .trim()
                .to_owned();
            if url.is_empty() {
                return fail(&s, 4, "endereço público não encontrado".into());
            }
            server = url.clone();
            signal = None;
            s.lock().unwrap().public_url = Some(url.clone());
            set_step(&s, 4, StepState::Done(url));
        } else {
            set_step(
                &s,
                4,
                StepState::Done("desligado (somente este computador)".into()),
            );
        }

        // 6./7. Applications (data per server instance, so a new server enrolls anew).
        let instance = server_key
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(10)
            .collect::<String>();
        let apps = paths
            .repo
            .join(".data")
            .join("apps")
            .join(format!("{}-{instance}", mode.slug()));
        let label = if internet {
            "Teste pela internet · servidor temporário de desenvolvimento".to_owned()
        } else {
            format!(
                "Validação local · servidor {} · não é produção",
                mode.label()
            )
        };
        set_step(&s, 5, StepState::Running);
        if internet {
            set_step(
                &s,
                5,
                StepState::Done("no computador do seu amigo (pacote gerado)".into()),
            );
        } else if process_running("aionix-host.exe") {
            set_step(&s, 5, StepState::Done("já estava aberto".into()));
        } else {
            let mut c = std::process::Command::new(paths.bin.join("aionix-host.exe"));
            c.arg("--server")
                .arg(&server)
                .arg("--data-dir")
                .arg(apps.join("host"))
                .arg("--environment-label")
                .arg(&label);
            if let Some(sig) = &signal {
                c.arg("--signal-url").arg(sig);
            }
            c.env("AIONIX_ENROLLMENT_TOKEN", &token);
            match c.spawn() {
                Ok(_) => set_step(&s, 6, StepState::Done("aberto".into())),
                Err(e) => return fail(&s, 6, e.to_string()),
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        set_step(&s, 6, StepState::Running);
        if process_running("aionix-technician.exe") {
            set_step(
                &s,
                6,
                StepState::Done(
                    "já estava aberto — feche-o e clique em Iniciar tudo para trocar de servidor"
                        .into(),
                ),
            );
        } else {
            let mut c = std::process::Command::new(paths.bin.join("aionix-technician.exe"));
            c.arg("--server")
                .arg(&server)
                .arg("--data-dir")
                .arg(apps.join("technician"))
                .arg("--email")
                .arg(&prov.email)
                .arg("--auto-login")
                .arg("--environment-label")
                .arg(&label)
                .env("AIONIX_DEV_PASSWORD", &prov.password);
            match c.spawn() {
                Ok(_) => set_step(&s, 6, StepState::Done("aberto".into())),
                Err(e) => return fail(&s, 6, e.to_string()),
            }
        }
        log(
            &s,
            "Pronto. No Técnico, clique em Conectar no dispositivo e responda Permitir na janela de confirmação.",
        );
        Ok(())
    }

    fn process_running(exe: &str) -> bool {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {exe}"), "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_lowercase()
                    .contains(&exe.to_lowercase())
            })
    }

    fn fail(s: &Shared, i: usize, e: String) -> Result<(), String> {
        log(s, format!("ERRO: {e}"));
        set_step(s, i, StepState::Failed(e.clone()));
        Err(e)
    }

    use std::os::windows::process::CommandExt;

    // ------------------------------------------------------------------ UI

    struct Launcher {
        state: Shared,
        rt: tokio::runtime::Runtime,
        paths: Option<Arc<Paths>>,
        auto: bool,
        show_password: bool,
    }

    impl Launcher {
        fn start(&mut self, ctx: &egui::Context) {
            let Some(paths) = self.paths.clone() else {
                return;
            };
            let mode = {
                let mut st = self.state.lock().unwrap();
                if st.running {
                    return;
                }
                st.running = true;
                st.started_at = Some(Instant::now());
                for step in &mut st.steps {
                    step.state = StepState::Pending;
                }
                st.log.clear();
                st.mode.unwrap_or(Mode::Native)
            };
            let (s, ctx) = (self.state.clone(), ctx.clone());
            self.rt.spawn(async move {
                let r = pipeline(s.clone(), paths, mode).await;
                if let Err(e) = r {
                    tracing::warn!(error = %e, "launcher pipeline failed");
                }
                s.lock().unwrap().running = false;
                ctx.request_repaint();
            });
        }

        fn stop_server(&mut self) {
            let Some(paths) = self.paths.clone() else {
                return;
            };
            let mode = self.state.lock().unwrap().mode.unwrap_or(Mode::Native);
            let s = self.state.clone();
            self.rt.spawn(async move {
                log(&s, "Parando o servidor...");
                let (script, file) = match mode {
                    Mode::Docker => ("infra/local/staging.ps1", "staging-down.log"),
                    Mode::Native => ("infra/local/native-stack.ps1", "native-down.log"),
                };
                let r = run_logged(
                    &s,
                    pwsh_script(&paths.repo, script, &["-Action", "Down"]),
                    &paths.dev.join("logs").join(file),
                    Duration::from_secs(120),
                )
                .await;
                log(
                    &s,
                    match r {
                        Ok(()) => "Servidor parado.".to_owned(),
                        Err(e) => format!("ERRO: {e}"),
                    },
                );
            });
        }
    }

    impl eframe::App for Launcher {
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            let p = theme::palette(ui);
            if self.auto && self.state.lock().unwrap().mode.is_some() {
                self.auto = false;
                self.start(ui.ctx());
            }
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(p.bg)
                        .inner_margin(egui::Margin::same(24)),
                )
                .show(ui, |ui| self.body(ui));
            ui.ctx().request_repaint_after(Duration::from_millis(400));
        }
    }

    impl Launcher {
        fn body(&mut self, ui: &mut egui::Ui) {
            let p = theme::palette(ui);
            ui.horizontal(|ui| {
                theme::brand_header(ui, "Dev Launcher · validação local");
            });
            ui.add_space(14.0);
            let Some(paths) = self.paths.clone() else {
                ui.label(RichText::new("Repositório do Aionix Connect não encontrado. Execute o launcher a partir da pasta do projeto ou use --repo.").color(p.danger));
                return;
            };
            let (running, mode, docker, creds) = {
                let st = self.state.lock().unwrap();
                (st.running, st.mode, st.docker_available, st.creds.clone())
            };

            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(theme::title("Servidor", 15.0));
                        let docker_text = match docker {
                            None => "verificando Docker...".to_owned(),
                            Some(true) => "Docker disponível".to_owned(),
                            Some(false) => "Docker indisponível".to_owned(),
                        };
                        ui.label(RichText::new(docker_text).size(12.5).color(p.muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_enabled_ui(!running, |ui| {
                            let mut m = mode.unwrap_or(Mode::Native);
                            let before = m;
                            ui.selectable_value(&mut m, Mode::Native, "Nativo");
                            ui.selectable_value(&mut m, Mode::Docker, "Docker");
                            if m != before {
                                self.state.lock().unwrap().mode = Some(m);
                            }
                        });
                    });
                });
                ui.add_enabled_ui(!running, |ui| {
                    let mut internet = self.state.lock().unwrap().internet;
                    if ui
                        .checkbox(&mut internet, "Testar pela internet com outro computador (túnel temporário)")
                        .changed()
                    {
                        let mut st = self.state.lock().unwrap();
                        st.internet = internet;
                        if internet {
                            st.mode = Some(Mode::Native);
                        }
                    }
                });
                if let Some(m) = mode {
                    let note = match m {
                        Mode::Docker => "PostgreSQL 17, Redis 7.4, API, signaling, relay e Caddy em containers — o caminho de staging suportado.",
                        Mode::Native => "Os mesmos binários do servidor como processos do Windows, com PostgreSQL 17 local e o Redis 3.0 da Microsoft. Somente para desenvolvimento.",
                    };
                    ui.label(RichText::new(note).size(12.5).color(p.muted));
                }
            });
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                if theme::primary_button(
                    ui,
                    if running {
                        "Iniciando..."
                    } else {
                        "Iniciar tudo"
                    },
                    !running && mode.is_some(),
                    Some(170.0),
                )
                .clicked()
                {
                    self.start(ui.ctx());
                }
                if theme::secondary_button(ui, "Parar servidor", !running, None).clicked() {
                    self.stop_server();
                }
                if theme::secondary_button(ui, "Abrir pasta de registros", true, None).clicked() {
                    let _ = std::process::Command::new("explorer")
                        .arg(paths.repo.join(".data"))
                        .spawn();
                }
            });
            ui.add_space(12.0);

            ui.columns(2, |cols| {
                let ui = &mut cols[0];
                theme::card(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::title("Etapas", 15.0));
                    ui.add_space(4.0);
                    let st = self.state.lock().unwrap();
                    for step in &st.steps {
                        ui.horizontal(|ui| {
                            let (sym, color) = match &step.state {
                                StepState::Pending => ("○", p.muted),
                                StepState::Running => ("", p.accent),
                                StepState::Done(_) => ("✔", p.success),
                                StepState::Failed(_) => ("✖", p.danger),
                            };
                            if step.state == StepState::Running {
                                ui.add(egui::Spinner::new().size(14.0).color(color));
                            } else {
                                ui.label(RichText::new(sym).color(color).size(14.0));
                            }
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.label(RichText::new(step.title).size(13.5));
                                match &step.state {
                                    StepState::Done(d) if !d.is_empty() => {
                                        ui.label(RichText::new(d).size(11.5).color(p.muted));
                                    }
                                    StepState::Failed(e) => {
                                        ui.label(RichText::new(e).size(11.5).color(p.danger));
                                    }
                                    _ => {}
                                }
                            });
                        });
                    }
                });
                let ui = &mut cols[1];
                theme::card(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::title("Conta de teste", 15.0));
                    match &creds {
                        Some(c) => {
                            theme::kv(ui, "E-mail", &c.email);
                            ui.horizontal(|ui| {
                                let shown = if self.show_password { c.password.clone() } else { "•".repeat(12) };
                                theme::kv(ui, "Senha", &shown);
                            });
                            ui.horizontal(|ui| {
                                if ui.small_button(if self.show_password { "Ocultar" } else { "Mostrar" }).clicked() {
                                    self.show_password = !self.show_password;
                                }
                                if ui.small_button("Copiar senha").clicked() {
                                    ui.ctx().copy_text(c.password.clone());
                                }
                            });
                            ui.label(RichText::new("O Técnico já abre com esta conta.").size(12.0).color(p.muted));
                        }
                        None => {
                            ui.label(RichText::new("Criada automaticamente ao iniciar.").size(12.5).color(p.muted));
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(theme::title("Como testar", 15.0));
                    for line in [
                        "1. No Técnico, clique em Conectar no dispositivo online.",
                        "2. Na janela \"Um técnico está solicitando acesso\", clique em Permitir.",
                        "3. A tela remota aparece no Técnico; use Diagnóstico para ver as métricas.",
                        "4. Encerre pelo Técnico ou pelo botão Encerrar sessão no computador atendido.",
                    ] {
                        ui.label(RichText::new(line).size(12.5));
                    }
                });
            });
            let public = self.state.lock().unwrap().public_url.clone();
            if let Some(url) = public {
                ui.add_space(12.0);
                theme::card(ui).stroke(Stroke::new(1.5, p.accent)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(theme::title("Teste com outro computador", 15.0));
                    theme::kv(ui, "Endereço público", &url);
                    let zip = paths.repo.join("dist").join("Aionix-Connect-Cliente.zip");
                    theme::kv(ui, "Pacote", &zip.display().to_string());
                    ui.label(RichText::new("Envie o pacote ao seu amigo. Ele extrai, abre \"Aionix Connect.exe\" e informa o ID; você digita o ID em Conectar por ID no Técnico.").size(12.5).color(p.muted));
                    ui.horizontal(|ui| {
                        if theme::secondary_button(ui, "Abrir pasta do pacote", true, None).clicked() {
                            let _ = std::process::Command::new("explorer").arg("/select,").arg(&zip).spawn();
                        }
                        if theme::secondary_button(ui, "Copiar endereço", true, None).clicked() {
                            ui.ctx().copy_text(url.clone());
                        }
                    });
                    ui.label(RichText::new("Enquanto o túnel estiver aberto o servidor de desenvolvimento fica acessível pela internet. Clique em Parar servidor ao terminar.").size(12.0).color(p.warning));
                });
            }
            ui.add_space(12.0);
            theme::card(ui)
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("Saída").size(12.5).color(p.muted));
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .max_height(ui.available_height())
                        .show(ui, |ui| {
                            let st = self.state.lock().unwrap();
                            for l in &st.log {
                                let color = if l.starts_with("ERRO") {
                                    p.danger
                                } else {
                                    p.muted
                                };
                                ui.label(RichText::new(l).monospace().size(11.5).color(color));
                            }
                        });
                });
            let _ = Stroke::NONE;
        }
    }
}
