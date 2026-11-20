use directories::ProjectDirs;
use montclair_protocol::http::CliListAccountsResponse;
use serde::Deserialize;
use serde::Serialize;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use montclair_observability::TError;
use montclair_protocol::message::AccountId;

pub struct UserConfig {
    path: PathBuf,
    fields: UserConfigFields,
}

#[derive(Serialize, Deserialize)]
pub struct UserConfigFields {
    pub personal_access_token_secret: Option<String>,
    pub account_id: Option<AccountId>,
    pub disable_telemetry: Option<bool>,
}

impl UserConfig {
    pub fn from_file(path: &Path) -> Result<Self, TError> {
        let s = std::fs::read_to_string(path)?;
        let fields = toml::from_str(&s)?;

        let config = Self {
            path: path.to_path_buf(),
            fields,
        };

        Ok(config)
    }

    pub fn fields(&self) -> &UserConfigFields {
        &self.fields
    }

    pub fn fields_mut(&mut self) -> &mut UserConfigFields {
        &mut self.fields
    }

    pub fn save(&self) -> Result<(), TError> {
        let contents = toml::to_string(&self.fields)?;
        std::fs::write(&self.path, contents)?;
        Ok(())
    }

    pub fn default_path() -> PathBuf {
        if let Some(proj_dirs) = ProjectDirs::from("com", "montclairci", "montclair_cli") {
            // Linux/FreeBSD: ~/.config/montclair_cli/config.toml
            // macOS:         ~/Library/Application Support/com.montclairci.montclair_cli/config.toml
            // Windows:       %USERPROFILE%\AppData\Roaming\montclairci\montclair_cli\config\config.toml
            proj_dirs.config_dir().join("config.toml")
        } else {
            // Fallback to current directory if home directory cannot be resolved
            PathBuf::from("config.toml")
        }
    }

    pub fn require_personal_access_token(&mut self) -> Result<String, TError> {
        if let Some(secret) = &self.fields.personal_access_token_secret {
            return Ok(secret.clone());
        }

        let url = "https://{server_hostname}/personal_access_tokens";

        eprintln!(
            "This command requires authentication. To authenticate, generate a Personal Access Token at the following URL:"
        );
        eprintln!();
        eprintln!("{url}");
        eprintln!();

        if !nix::unistd::isatty(std::io::stdin()).unwrap_or(false) {
            eprintln!("Copy your personal access token.");
            eprintln!("Open your config file: [{}]", self.path.display());
            eprintln!("Add this line:");
            eprintln!("personal_access_token = do_not_share_this_token");
            eprintln!();

            return Err(TError::new("Login required"));
        }

        eprintln!("Copy your personal access token, paste it here, and hit ENTER.");
        eprintln!();
        eprintln!("Personal Access Token secret: ");
        std::io::stdout().flush()?;

        let secret = rpassword::read_password()?;
        self.fields_mut().personal_access_token_secret = Some(secret.clone());
        self.save()?;

        Ok(secret)
    }

    pub async fn require_account_id(
        &mut self,
        server_hostname: &str,
        secret: &str,
    ) -> Result<AccountId, TError> {
        if let Some(account_id) = &self.fields.account_id {
            return Ok(*account_id);
        }

        if !nix::unistd::isatty(std::io::stdin()).unwrap_or(false) {
            eprintln!(
                "This command requires Account selection. To configure your Account selection, add an Account ID to your configuration file. You may view your Account IDs at the following URL:"
            );
            eprintln!();
            eprintln!("https://{server_hostname}/cli_accounts");
            eprintln!();

            if !nix::unistd::isatty(std::io::stdin()).unwrap_or(false) {
                eprintln!("Copy your selected Account ID.");
                eprintln!("Open your config file: [{}]", self.path.display());
                eprintln!("Add this line:");
                eprintln!("account_id = your_account_id");
                eprintln!();

                return Err(TError::new("Login required"));
            }
        }

        let url = "https://{server_hostname}/cli_accounts";

        let response = reqwest::Client::new()
            .get(url)
            .header("x-montclair-personal-access-token-secret", secret)
            .header("accept", "application/json")
            .send()
            .await?;

        if !response.status().is_success() {
            let body = response.text().await?;
            return Err(TError::new(&format!("failed to fetch accounts: {body}")));
        }

        let accounts = response.json::<CliListAccountsResponse>().await?.accounts;

        if accounts.is_empty() {
            return Err(TError::new("no accounts found"));
        }

        if accounts.len() == 1 {
            return Ok(accounts[0].account_id);
        }

        println!("You are associated with multiple Accounts.");
        println!("Which Account would you like to select?:");

        for (i, account) in accounts.iter().enumerate() {
            println!("{}: {} ({})", i + 1, account.name, account.account_id);
        }

        print!("Enter your choice: ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let choice: usize = input.trim().parse()?;

        if choice < 1 || choice > accounts.len() {
            eprintln!("invalid choice: {choice}");
            return Err(TError::new("invalid account selection"));
        }

        let account_id = accounts[choice - 1].account_id;

        Ok(account_id)
    }
}
