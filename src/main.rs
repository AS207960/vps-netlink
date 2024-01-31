#[macro_use]
extern crate log;

use clap::Parser;

mod config;
mod netlink;
mod diff;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    config: std::path::PathBuf,
    #[arg(long)]
    templates: String,
    #[arg(long)]
    radvd: std::path::PathBuf,
    #[arg(long)]
    kea: std::path::PathBuf,
}

#[derive(Debug)]
enum Error {
    Netlink(rtnetlink::Error),
    Tera(tera::Error),
    Io(std::io::Error),
    InterfaceNotFound(String),
}

impl From<rtnetlink::Error> for Error {
    fn from(value: rtnetlink::Error) -> Self {
        match value {
            rtnetlink::Error::NetlinkError(e) => {
                Self::Io(e.to_io())
            }
            v => Self::Netlink(v)
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<tera::Error> for Error {
    fn from(value: tera::Error) -> Self {
        Self::Tera(value)
    }
}

async fn handle_signals(
    mut signals: tokio::signal::unix::Signal,
    config_path: std::path::PathBuf,
    config: std::sync::Arc<tokio::sync::Mutex<config::Config>>
) {
    while let Some(()) = signals.recv().await {
        let config_file = match tokio::fs::read(&config_path).await {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to open config file: {}", e);
                continue;
            }
        };
        let new_config: config::Config = match serde_json::from_slice(&config_file) {
            Ok(c) => c,
            Err(e) => {
                error!("Failed to parse config file: {}", e);
                continue;
            }
        };
        *config.lock().await = new_config;
        info!("Config reloaded");
    }
}

struct ConfigPaths<'a> {
    radvd: &'a std::path::Path,
    kea: &'a std::path::Path,
}

async fn update(
    handle: &rtnetlink::Handle,
    templates: &tera::Tera,
    config: &config::Config,
    config_paths: ConfigPaths<'_>,
    first_update: bool,
) -> Result<bool, Error> {
    let state = netlink::get_state(&handle, config.rt_proto).await?;
    let (diff, interfaces) = diff::make_diff(&handle, &config.interface, &config.vps, state).await?;

    if !diff.is_empty() || first_update {
        info!("Updating interfaces");
        diff::apply_diff(&handle, config.rt_proto, diff).await?;
        update_config(templates, "radvd.tera", config_paths.radvd, &interfaces).await?;
        update_config(templates, "kea.tera", config_paths.kea, &interfaces).await?;

        Ok(true)
    } else {
        Ok(false)
    }
}

async fn update_config(
    templates: &tera::Tera,
    template: &str,
    config_file: &std::path::Path,
    interfaces: &[diff::InterfaceState<'_>]
) -> Result<(), Error>  {
    let mut context = tera::Context::new();
    context.insert("interfaces", interfaces);
    let config = templates.render(template, &context)?;
    tokio::fs::write(config_file, config).await?;
    Ok(())
}

async fn run_radvd(
    radvd_path: &std::path::Path,
    config_path: &std::path::Path,
    pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        info!("Starting radvd");
        let mut command = tokio::process::Command::new(radvd_path);
        command.arg("--nodaemon");
        command.arg("--logmethod=stderr");
        command.arg("-C");
        command.arg(config_path);

        let mut handle = match command.spawn() {
            Ok(h) => h,
            Err(err) => {
                error!("Failed to start radvd: {}", err);
                continue
            }
        };
        pid.store(handle.id().unwrap(), std::sync::atomic::Ordering::Relaxed);
        match handle.wait().await {
            Ok(s) => {
                if !s.success() {
                    warn!("radvd exited with code: {}", s);
                }
            }
            Err(err) => {
                error!("radvd failed: {}", err);
            }
        }
    }
}

async fn run_kea(
    kea_path: &std::path::Path,
    config_path: &std::path::Path,
    pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
) -> Result<(), Error> {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        info!("Starting kea");
        let mut command = tokio::process::Command::new(kea_path);
        command.arg("-c");
        command.arg(config_path);
        command.env("KEA_PIDFILE_DIR", "/run");

        let mut handle = match command.spawn() {
            Ok(h) => h,
            Err(err) => {
                error!("Failed to start kea: {}", err);
                continue
            }
        };
        pid.store(handle.id().unwrap(), std::sync::atomic::Ordering::Relaxed);
        match handle.wait().await {
            Ok(s) => {
                if !s.success() {
                    warn!("kea exited with code: {}", s);
                }
            }
            Err(err) => {
                error!("kea failed: {}", err);
            }
        }
    }
}

#[tokio::main]
async fn main() {
    pretty_env_logger::init();
    let args = Args::parse();

    let tera = tera::Tera::new(&args.templates).expect("Unable to setup Tera");

    let signals = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).expect("Unable to create signal listener");

    let config_file = tokio::fs::read(&args.config).await.expect("Unable to open config file");
    let config: config::Config = serde_json::from_slice(&config_file).expect("Unable to parse config file");
    info!("Config loaded");

    let radvd_config_file = tempfile::Builder::new()
        .prefix("radvd")
        .tempfile().expect("Unable to create radvd config file");
    let kea_config_file = tempfile::Builder::new()
        .prefix("kea")
        .tempfile().expect("Unable to create kea config file");

    let (conn, handle, mut _messages) = rtnetlink::new_connection().expect("Unable to open netlink");
    tokio::spawn(conn);

    if let Err(err) = update(&handle, &tera, &config, ConfigPaths {
        radvd: radvd_config_file.path(),
        kea: kea_config_file.path(),
    }, true).await {
        error!("Failed to run first update: {:?}", err);
        return;
    }

    let radvd_config_file_path = radvd_config_file.path().to_path_buf();
    let kea_config_file_path = kea_config_file.path().to_path_buf();
    let radvd_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let kea_pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let radvd_pid_1 = radvd_pid.clone();
    let kea_pid_1 = kea_pid.clone();
    tokio::task::spawn(async move {
        run_radvd(&args.radvd, &radvd_config_file_path, radvd_pid_1).await;
    });
    tokio::task::spawn(async move {
        run_kea(&args.kea, &kea_config_file_path, kea_pid_1).await.expect("Unable to start kea");
    });

    let config = std::sync::Arc::new(tokio::sync::Mutex::new(config));

    tokio::spawn(handle_signals(signals, args.config.clone(), config.clone()));

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let config = config.lock().await;
        let did_update = match update(&handle, &tera, &config, ConfigPaths {
            radvd: radvd_config_file.path(),
            kea: kea_config_file.path(),
        }, false).await {
            Ok(d) => d,
            Err(err) => {
                error!("Failed to run update: {:?}", err);
                continue;
            }
        };
        drop(config);
        if did_update {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let radvd_pid = nix::unistd::Pid::from_raw(radvd_pid.load(std::sync::atomic::Ordering::Relaxed) as i32);
            if let Err(err) = nix::sys::signal::kill(radvd_pid, nix::sys::signal::Signal::SIGHUP) {
                warn!("Failed to reload radvd: {}", err);
            }
            let kea_pid = nix::unistd::Pid::from_raw(kea_pid.load(std::sync::atomic::Ordering::Relaxed) as i32);
            if let Err(err) = nix::sys::signal::kill(kea_pid, nix::sys::signal::Signal::SIGHUP) {
                warn!("Failed to reload kea: {}", err);
            }
        }
    }
}
