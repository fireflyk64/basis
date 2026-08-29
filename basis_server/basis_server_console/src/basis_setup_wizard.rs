//! Port of `BasisSetupWizard.cs`.
//!
//! First-boot setup wizard. Runs once, before the network server starts, when no config.xml
//! existed at launch (a brand-new server). It walks the operator through the core server
//! settings and asks them to designate at least one admin, so a fresh server is never left
//! misconfigured or with nobody able to moderate it. Admins are stored in permissions.xml as
//! members of the "admin" group, exactly like the runtime `/perm user group add <uuid> admin`
//! command; the other settings are written straight back to config.xml.

use std::io::Write;
use std::path::Path;

use basis_network_core::BNL;
use basis_network_core::configuration::Configuration;
use basis_network_server::security::permission_manager::{PermissionIntegration, PermissionManager};

pub struct BasisSetupWizard;

impl BasisSetupWizard {
    /// Environment variable used to seed admins on headless/automated first boots (Docker, CI)
    /// where no interactive console is attached. Accepts one UUID or a comma/space separated list.
    pub const ADMIN_ENV_VAR: &'static str = "BasisFirstAdmin";
    const ADMIN_GROUP: &'static str = "admin";
    const DEFAULT_PASSWORD: &'static str = "default_password";

    pub fn run(config: &mut Configuration, config_file_path: &Path) {
        let config_dir = config_file_path.parent().unwrap_or(Path::new(""));
        let permissions_path = config_dir.join("permissions.xml");

        // Bring the shared permission store up far enough that the "admin" group exists, then
        // write straight to permissions.xml. When the server boots a moment later,
        // PermissionIntegration::init reloads this same file, so the admin we add sticks.
        let pm = PermissionIntegration::manager();
        if let Err(e) = pm.set_xml_path(&permissions_path) {
            BNL::log_warning(format!("[Setup] Could not point the permission store at {}: {}", permissions_path.display(), e.report()));
        }
        if let Err(e) = pm.load_from_xml(None) {
            // A first boot has no permissions.xml yet; anything else is worth saying.
            if permissions_path.exists() {
                BNL::log_warning(format!("[Setup] Could not read {}: {}", permissions_path.display(), e.report()));
            }
        }
        pm.ensure_defaults();

        // Headless / automated deploys: seed admins from the env var instead of prompting.
        if let Some(from_env) = std::env::var(Self::ADMIN_ENV_VAR).ok().filter(|v| !v.trim().is_empty()) {
            let seeded = Self::seed_from_list(pm, &from_env);
            if seeded > 0 {
                Self::save(pm);
                BNL::log(format!("[Setup] First boot: added {seeded} admin(s) from ${}.", Self::ADMIN_ENV_VAR));
                return;
            }
            BNL::log_warning(format!("[Setup] ${} was set but contained no usable UUIDs.", Self::ADMIN_ENV_VAR));
        }

        if !Self::can_prompt() {
            Self::warn_no_admin();
            return;
        }

        Self::run_interactive(pm, config, config_file_path);
    }

    fn run_interactive(pm: &PermissionManager, config: &mut Configuration, config_file_path: &Path) {
        Self::print_intro();
        Self::run_settings_walkthrough(config, config_file_path);
        let admins = Self::prompt_admins(pm);
        BNL::log(if admins == 0 {
            "[Setup] First-time setup complete - no admin configured. Starting server...".to_string()
        } else {
            format!("[Setup] First-time setup complete - {admins} admin(s) configured. Starting server...")
        });
    }

    // ----------------------------------------------------------------------------
    // Core server settings
    // ----------------------------------------------------------------------------

    fn run_settings_walkthrough(config: &mut Configuration, config_file_path: &Path) {
        BNL::log("");
        BNL::log("--- Server settings ---");
        BNL::log("Press Enter to keep the [current] value, or type a new one.");
        BNL::log("");

        let name = Self::prompt_string("Server name (shown in the server list)", &config.server_name);
        let motd = Self::prompt_string("Message of the day / MOTD", &config.server_motd);
        let port = Self::prompt_ushort("Game port (UDP)", config.set_port);
        let password = Self::prompt_password(&config.password);
        let peer_limit = Self::prompt_int("Max players", config.peer_limit, 1);

        BNL::log("");
        BNL::log("--- Review ---");
        BNL::log(format!("  Server name : {name}"));
        BNL::log(format!("  MOTD        : {}", if motd.is_empty() { "<none>" } else { &motd }));
        BNL::log(format!("  Game port   : {port}"));
        BNL::log(format!("  Password    : {}", if password == Self::DEFAULT_PASSWORD { Self::DEFAULT_PASSWORD } else { "(set)" }));
        BNL::log(format!("  Max players : {peer_limit}"));
        BNL::log("");

        if !Self::confirm_default_yes("Save these settings and start the server?") {
            BNL::log("[Setup] Settings discarded - keeping config.xml defaults.");
            return;
        }

        config.server_name = name;
        config.server_motd = motd;
        config.set_port = port;
        config.password = password;
        config.peer_limit = peer_limit;
        match config.save_to_xml(config_file_path) {
            Ok(()) => BNL::log("[Setup] Settings saved to config.xml."),
            Err(e) => BNL::log_error(format!("[Setup] Settings could not be saved to {}: {e}", config_file_path.display())),
        }
    }

    // ----------------------------------------------------------------------------
    // Admin
    // ----------------------------------------------------------------------------

    fn prompt_admins(pm: &PermissionManager) -> i32 {
        BNL::log("");
        BNL::log("--- Admin setup ---");
        Self::print_admin_help();

        let mut admin_count = 0;
        loop {
            Self::ask(if admin_count == 0 { "Admin player UUID ('skip' to start without one): " } else { "Add another admin UUID (leave blank to finish): " });
            let input = Self::read_answer();

            if input.eq_ignore_ascii_case("help") || input == "?" {
                Self::print_admin_help();
                continue;
            }

            if input.is_empty() || input.eq_ignore_ascii_case("skip") {
                if admin_count > 0 {
                    break;
                }
                if !Self::confirm("Start this server with no admin? Nobody will be able to moderate or configure it in game.") {
                    continue;
                }
                Self::warn_skipped_admin();
                break;
            }

            if !Self::looks_like_did(&input) && !Self::confirm(&format!("'{input}' doesn't look like a did:key UUID. Add it anyway?")) {
                continue;
            }

            pm.add_user_to_group(&input, Self::ADMIN_GROUP);
            Self::save(pm);
            admin_count += 1;
            BNL::log(format!("[Setup] Added admin: {input}"));
        }
        admin_count
    }

    /// Add every UUID in a delimited list to the admin group. Returns how many were added.
    fn seed_from_list(pm: &PermissionManager, raw: &str) -> i32 {
        let mut added = 0;
        for part in raw.split([',', ';', ' ', '\t', '\r', '\n']) {
            let uuid = part.trim();
            if uuid.is_empty() {
                continue;
            }
            pm.add_user_to_group(uuid, Self::ADMIN_GROUP);
            BNL::log(format!("[Setup] Added admin: {uuid}"));
            added += 1;
        }
        added
    }

    fn save(pm: &PermissionManager) {
        if let Err(e) = pm.save_to_xml(None) {
            BNL::log_error(format!("[Setup] permissions.xml could not be saved: {}", e.report()));
        }
    }

    // ----------------------------------------------------------------------------
    // Prompt helpers
    // ----------------------------------------------------------------------------

    fn prompt_string(label: &str, current: &str) -> String {
        let shown = if current.is_empty() { "<none>" } else { current };
        Self::ask(&format!("{label} [{shown}]: "));
        let input = Self::read_answer();
        if input.is_empty() { current.to_string() } else { input }
    }

    fn prompt_ushort(label: &str, current: u16) -> u16 {
        loop {
            Self::ask(&format!("{label} [{current}]: "));
            let input = Self::read_answer();
            if input.is_empty() {
                return current;
            }
            if let Ok(value) = input.parse::<u16>() {
                return value;
            }
            BNL::log_warning(format!("Please enter a whole number between 0 and {}.", u16::MAX));
        }
    }

    fn prompt_int(label: &str, current: i32, min: i32) -> i32 {
        loop {
            Self::ask(&format!("{label} [{current}]: "));
            let input = Self::read_answer();
            if input.is_empty() {
                return current;
            }
            if let Ok(value) = input.parse::<i32>()
                && value >= min
            {
                return value;
            }
            BNL::log_warning(format!("Please enter a whole number of {min} or more."));
        }
    }

    fn prompt_password(current: &str) -> String {
        Self::ask("Server password [keep current]: ");
        let input = Self::read_answer();
        if input.is_empty() { current.to_string() } else { input }
    }

    // ----------------------------------------------------------------------------
    // Text / confirmation
    // ----------------------------------------------------------------------------

    fn print_intro() {
        BNL::log("============================================================");
        BNL::log(" Basis Server - First-Time Setup");
        BNL::log("============================================================");
        BNL::log("No config.xml was found, so this is a brand-new server.");
        BNL::log("This quick wizard sets up the core server settings and has");
        BNL::log("you designate an admin before the server starts.");
    }

    fn print_admin_help() {
        BNL::log("An admin is identified by their Basis player UUID (a DID), which");
        BNL::log("looks like:  did:key:z6Mk...");
        BNL::log("Where to find it:");
        BNL::log("  - in the Basis client: open Settings > Developer tab, find the");
        BNL::log("    \"Identity Key\" section, and tap the eye icon on the \"UUID\"");
        BNL::log("    field to reveal it; or");
        BNL::log("  - in this server's log: every time a player connects it prints");
        BNL::log("    their UUID as \"(UUID did:key:...)\".");
        BNL::log("The UUID you enter is added to the \"admin\" group (full access).");
        BNL::log("You can add more than one; leave the prompt blank once done.");
        BNL::log("Don't have it to hand? Type 'skip' to start without an admin and");
        BNL::log("add one later with:  /perm user group add <uuid> admin");
        BNL::log("");
    }

    fn warn_skipped_admin() {
        BNL::log_warning("");
        BNL::log_warning("============================================================");
        BNL::log_warning(" Starting with NO ADMIN");
        BNL::log_warning("------------------------------------------------------------");
        BNL::log_warning(" Nobody can moderate or configure this server in game.");
        BNL::log_warning(" Add one at any time from this console with:");
        BNL::log_warning("   /perm user group add <uuid> admin");
        BNL::log_warning("============================================================");
    }

    fn warn_no_admin() {
        BNL::log_warning("============================================================");
        BNL::log_warning(" Basis Server first-time setup: NO ADMIN CONFIGURED");
        BNL::log_warning("------------------------------------------------------------");
        BNL::log_warning(" No interactive console is attached and the environment");
        BNL::log_warning(format!(" variable {} is not set, so the server is starting", Self::ADMIN_ENV_VAR));
        BNL::log_warning(" WITHOUT an admin - nobody can moderate or configure it.");
        BNL::log_warning(" To set one up, provide the admin's player UUID, e.g.:");
        BNL::log_warning(format!("   {}=did:key:z6Mk...   (comma-separate for several)", Self::ADMIN_ENV_VAR));
        BNL::log_warning(" then delete config/config.xml and restart, or run the server");
        BNL::log_warning(" once in an interactive terminal to use the setup wizard.");
        BNL::log_warning("============================================================");
    }

    pub fn looks_like_did(value: &str) -> bool {
        // `value` is operator input, so it may split a multi-byte character at byte 4; slicing
        // there would panic and abort the first-boot wizard. `starts_with` is char-safe.
        value.len() > "did:".len() && value.get(.."did:".len()).is_some_and(|p| p.eq_ignore_ascii_case("did:")) && !value.contains(' ')
    }

    /// Yes/no prompt that defaults to NO on a blank line.
    fn confirm(question: &str) -> bool {
        Self::ask(&format!("{question} (y/N): "));
        let answer = Self::read_answer();
        answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    }

    /// Yes/no prompt that defaults to YES on a blank line.
    fn confirm_default_yes(question: &str) -> bool {
        Self::ask(&format!("{question} (Y/n): "));
        let answer = Self::read_answer();
        answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    }

    /// True only when a real interactive terminal is attached (so a prompt won't hang a daemon).
    fn can_prompt() -> bool {
        #[cfg(unix)]
        {
            // SAFETY: isatty only inspects a descriptor.
            unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    fn ask(prompt: &str) {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "{prompt}");
        let _ = out.flush();
    }

    fn read_answer() -> String {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        line.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_detection() {
        assert!(BasisSetupWizard::looks_like_did("did:key:z6MkabcDEF"));
        assert!(BasisSetupWizard::looks_like_did("DID:web:example.com"));
        assert!(!BasisSetupWizard::looks_like_did("did:"));
        assert!(!BasisSetupWizard::looks_like_did("did: key"));
        assert!(!BasisSetupWizard::looks_like_did("z6Mkabc"));
        assert!(!BasisSetupWizard::looks_like_did(""));
    }
}
