//! `aionix-admin` — operator CLI: migrations, key generation, bootstrap of
//! organizations/accounts, enrollment tokens, revocation and status.
//!
//! Passwords are never accepted as command-line arguments (they would end up in shell
//! history and process listings): they are read from `AIONIX_NEW_USER_PASSWORD` or
//! from the first line of standard input.

use std::{io::BufRead, path::PathBuf, process::ExitCode, sync::Arc};

use aionix_control_api::rest::OrgRole;
use aionix_control_plane::{
    admin,
    bus::{MemoryBus, RedisBus, SignalBus},
    db, devices,
    presence::{PresenceStore, RedisPresence},
    redis_conn::{self, Keys},
};
use aionix_server_common::keys::{load_signing_key, write_new_signing_key};
use clap::{Parser, Subcommand};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "aionix-admin", version, about = "Aionix Connect server administration")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Apply pending database migrations (idempotent).
    Migrate,
    /// Report whether every migration is applied (exit 1 otherwise).
    MigrationStatus,
    /// Create a new token signing key file (never overwrites).
    Keygen {
        #[arg(long)]
        out: PathBuf,
    },
    /// Print the public key of a signing key file (for AIONIX_TOKEN_PUBLIC_KEYS).
    PublicKey {
        #[arg(long)]
        key_file: PathBuf,
    },
    /// Create an organization; prints its id.
    CreateOrg {
        #[arg(long)]
        name: String,
    },
    /// Create a user account (password from AIONIX_NEW_USER_PASSWORD or stdin).
    CreateUser {
        #[arg(long)]
        email: String,
        #[arg(long)]
        name: String,
    },
    /// Add a user to an organization or change their role.
    AddMember {
        #[arg(long)]
        org: Uuid,
        #[arg(long)]
        email: String,
        /// viewer | technician | admin | owner
        #[arg(long)]
        role: String,
    },
    /// Create a device enrollment token (shown once).
    EnrollmentToken {
        #[arg(long)]
        org: Uuid,
        #[arg(long, default_value_t = 24)]
        hours: i64,
        #[arg(long, default_value_t = 1)]
        uses: i32,
    },
    /// Disable an account and revoke its logins.
    DisableUser {
        #[arg(long)]
        email: String,
    },
    /// Revoke a device identity.
    RevokeDevice {
        #[arg(long)]
        device: Uuid,
        #[arg(long, default_value = "revoked by operator")]
        reason: String,
    },
    /// Counts used by the diagnostic script.
    Status {
        #[arg(long)]
        json: bool,
    },
}

fn env_required(name: &str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("{name} is not set"))
}

fn read_password() -> Result<String, String> {
    if let Ok(p) = std::env::var("AIONIX_NEW_USER_PASSWORD")
        && !p.is_empty()
    {
        return Ok(p);
    }
    eprintln!("Password (one line on stdin):");
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let p = line.trim_end_matches(['\r', '\n']).to_owned();
    if p.is_empty() {
        return Err("no password given".into());
    }
    Ok(p)
}

async fn pool() -> Result<sqlx::PgPool, String> {
    let url = env_required("AIONIX_DATABASE_URL")?;
    db::connect(&url, 2, 10).await.map_err(|e| e.to_string())
}

async fn redis() -> Option<(redis::Client, redis::aio::ConnectionManager, Keys)> {
    let url = std::env::var("AIONIX_REDIS_URL").ok()?;
    let prefix = std::env::var("AIONIX_REDIS_PREFIX").unwrap_or_else(|_| "aionix".into());
    match redis_conn::connect(&url, 3).await {
        Ok((c, m)) => Some((c, m, Keys::new(prefix))),
        Err(e) => {
            eprintln!("warning: Redis unavailable ({e})");
            None
        }
    }
}

async fn run(cmd: Cmd) -> Result<(), String> {
    match cmd {
        Cmd::Keygen { out } => {
            let key = write_new_signing_key(&out).map_err(|e| e.to_string())?;
            println!("{}", key.public().to_base64());
            eprintln!(
                "Signing key written to {} (keep it secret; back it up with the server backups).",
                out.display()
            );
        }
        Cmd::PublicKey { key_file } => {
            let key = load_signing_key(&key_file).map_err(|e| e.to_string())?;
            println!("{}", key.public().to_base64());
        }
        Cmd::Migrate => {
            let db = pool().await?;
            db::migrate(&db).await.map_err(|e| e.to_string())?;
            println!("migrations applied");
        }
        Cmd::MigrationStatus => {
            let db = pool().await?;
            db::check_ready(&db).await?;
            println!("schema up to date");
        }
        Cmd::CreateOrg { name } => {
            let db = pool().await?;
            let id = admin::create_org(&db, &name).await.map_err(|e| e.to_string())?;
            println!("{id}");
        }
        Cmd::CreateUser { email, name } => {
            let password = read_password()?;
            let db = pool().await?;
            let id = admin::create_user(&db, &email, &name, password)
                .await
                .map_err(|e| e.to_string())?;
            println!("{id}");
        }
        Cmd::AddMember { org, email, role } => {
            let role = OrgRole::parse(&role).ok_or("role must be viewer|technician|admin|owner")?;
            let db = pool().await?;
            let user = admin::user_id_by_email(&db, &email)
                .await
                .map_err(|e| e.to_string())?;
            admin::set_membership(&db, org, user, role)
                .await
                .map_err(|e| e.to_string())?;
            println!("{email} is {} of {org}", role.as_str());
        }
        Cmd::EnrollmentToken { org, hours, uses } => {
            if !(1..=720).contains(&hours) || !(1..=1000).contains(&uses) {
                return Err("hours must be 1..=720 and uses 1..=1000".into());
            }
            let db = pool().await?;
            let (_, token, expires) = admin::insert_enrollment_token(&db, org, None, hours, uses, None)
                .await
                .map_err(|e| e.to_string())?;
            println!("{token}");
            eprintln!("valid until {expires} for {uses} device(s); shown only once");
        }
        Cmd::DisableUser { email } => {
            let db = pool().await?;
            let user = admin::user_id_by_email(&db, &email)
                .await
                .map_err(|e| e.to_string())?;
            let n = admin::disable_user(&db, user).await.map_err(|e| e.to_string())?;
            println!("account disabled; {n} login session(s) revoked");
        }
        Cmd::RevokeDevice { device, reason } => {
            let db = pool().await?;
            let r = redis().await;
            let bus: Arc<dyn SignalBus> = match &r {
                Some((c, m, k)) => Arc::new(RedisBus::new(c.clone(), m.clone(), k.clone())),
                None => Arc::new(MemoryBus::new()),
            };
            let redis_pair = r.as_ref().map(|(_, m, k)| (m.clone(), k.clone()));
            let done = devices::revoke(&db, bus.as_ref(), redis_pair.as_ref(), device, None, &reason, (None, None))
                .await
                .map_err(|e| e.to_string())?;
            println!("{}", if done { "device revoked" } else { "device was already revoked (or does not exist)" });
        }
        Cmd::Status { json } => {
            let db = pool().await?;
            let r = redis().await;
            let presence = r
                .as_ref()
                .map(|(_, m, k)| RedisPresence::new(m.clone(), k.clone()));
            let st = admin::status(&db, presence.as_ref().map(|p| p as &dyn PresenceStore))
                .await
                .map_err(|e| e.to_string())?;
            if json {
                println!("{}", serde_json::to_string(&st).map_err(|e| e.to_string())?);
            } else {
                println!("organizations     {}", st.organizations);
                println!("users             {}", st.users);
                println!("devices           {} (revoked {})", st.devices, st.devices_revoked);
                println!(
                    "devices online    {}",
                    st.devices_online.map(|n| n.to_string()).unwrap_or_else(|| "unavailable".into())
                );
                println!("sessions active   {}", st.sessions_active);
                println!("sessions 24h      {}", st.sessions_last_24h);
                println!("schema ready      {}", st.schema_ready);
            }
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.cmd).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
