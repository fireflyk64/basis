//! Port of `BasisConsoleCommands.cs`: the command registry, the `/perm` and `/config` families
//! and the console reader thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use basis_network_core::BNL;
use basis_network_core::configuration::{BasisXmlConfig, Configuration};
use basis_network_server::NetworkServer;
use basis_network_server::networking::basis_saved_state::BasisSavedState;
use basis_network_server::security::permission_manager::{PermissionIntegration, PermissionManager};
use parking_lot::Mutex;

use crate::Program;
use crate::basis_console_driver::BasisConsoleDriver;

pub type CommandHandler = Arc<dyn Fn(&[String]) + Send + Sync>;

/// Command class to store command info.
#[derive(Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    pub handler: CommandHandler,
}

static COMMANDS: Mutex<Vec<Command>> = Mutex::new(Vec::new());

pub struct BasisConsoleCommands;

impl BasisConsoleCommands {
    /// Passed to the process /restart launches so it waits for its predecessor to release the port.
    pub const AWAIT_PID_ARGUMENT: &'static str = "--await-pid=";

    // Registering commands
    pub fn register_command(command_name: &str, description: &str, handler: impl Fn(&[String]) + Send + Sync + 'static) {
        let key = command_name.to_lowercase();
        let command = Command { name: command_name.to_string(), description: description.to_string(), handler: Arc::new(handler) };
        let mut commands = COMMANDS.lock();
        match commands.iter_mut().find(|c| c.name.to_lowercase() == key) {
            Some(existing) => *existing = command,
            None => commands.push(command),
        }
    }

    fn find_command(key: &str) -> Option<Command> {
        COMMANDS.lock().iter().find(|c| c.name.to_lowercase() == key).cloned()
    }

    /// Every registered command, in registration order.
    pub fn commands() -> Vec<Command> {
        COMMANDS.lock().clone()
    }

    /// Test seam: forgets every registered command.
    #[cfg(test)]
    pub fn clear_commands() {
        COMMANDS.lock().clear();
    }

    // ── /config ──

    /// Register commands for each configuration field.
    pub fn register_configuration_commands() {
        Self::register_command("/config", "Lists every server setting. /config <name> [value] to read or change one.", Self::handle_config_root);
        for field in Configuration::field_names() {
            let command_name = format!("/config {}", field.to_lowercase());
            let field_name = field.to_string();
            Self::register_command(&command_name, "", move |args| Self::handle_config_field(args, &field_name));
        }
    }

    fn handle_config_root(args: &[String]) {
        let fields = Configuration::field_names();
        if args.is_empty() {
            let config = NetworkServer::configuration_or_default();
            BNL::log(format!("{} settings. '*' takes effect on /restart, '+' applies to new joins only.", fields.len()));
            for field in fields {
                let marker = if Configuration::requires_restart(field) {
                    "*"
                } else if Configuration::applies_to_new_joins_only(field) {
                    "+"
                } else {
                    " "
                };
                BNL::log(format!(" {marker} {field} = {}", Self::display_value(field, &config)));
            }
            return;
        }
        let Some(field) = fields.iter().find(|f| f.eq_ignore_ascii_case(&args[0])) else {
            BNL::log(format!("Unknown setting '{}'. Type /config to list them.", args[0]));
            return;
        };
        Self::handle_config_field(&args[1..], field);
    }

    fn display_value(field: &str, config: &Configuration) -> String {
        let raw = config.get_field(field).unwrap_or_default();
        if Configuration::is_secret_field_name(field) {
            return if raw.is_empty() { "<empty>".to_string() } else { "<redacted>".to_string() };
        }
        raw
    }

    pub fn handle_config_field(args: &[String], field: &str) {
        if args.is_empty() {
            let config = NetworkServer::configuration_or_default();
            let suffix = if Configuration::requires_restart(field) {
                "  (takes effect on /restart)"
            } else if Configuration::applies_to_new_joins_only(field) {
                "  (applies to new joins only)"
            } else {
                ""
            };
            BNL::log(format!("{field}: {}{suffix}", Self::display_value(field, &config)));
            return;
        }

        // Rejoined rather than args[0]: ServerMotd and ServerName carry spaces, and splitting
        // them on the command line silently truncated the value to its first word.
        let new_value = args.join(" ");
        let mut updated = (*NetworkServer::configuration_or_default()).clone();
        if let Err(e) = updated.set_field(field, &new_value) {
            BNL::log(format!("Failed to set {field} to '{new_value}'. {e}"));
            return;
        }

        // The change is persisted before it is applied; a save that fails leaves the running
        // configuration untouched, which is the C# "revert" without the window in between.
        if let Err(e) = updated.save_to_xml(&Configuration::get_default_path()) {
            BNL::log_error(format!("Failed to persist {field}, change reverted: {e}"));
            return;
        }
        NetworkServer::set_configuration(updated);

        let shown = if Configuration::is_secret_field_name(field) { "<redacted>".to_string() } else { new_value };
        if Configuration::requires_restart(field) {
            BNL::log(format!("Set {field} to {shown}. Saved — takes effect on /restart."));
            return;
        }
        NetworkServer::apply_live_configuration();
        BNL::log(if Configuration::applies_to_new_joins_only(field) {
            format!("Set {field} to {shown}. Saved and applied to new joins.")
        } else {
            format!("Set {field} to {shown}. Saved and applied live.")
        });
    }

    // ── /perm ──

    pub fn register_permission_commands() {
        // Root help
        Self::register_command("/perm", "Permission system commands. Type /perm help", Self::handle_perm_root);
        Self::register_command("/perm help", "Shows permission command help", Self::handle_perm_help);

        // IO / path
        Self::register_command("/perm path", "Shows current permissions.xml path", Self::handle_perm_path);
        Self::register_command("/perm path set", "Sets permissions.xml path (no load). Usage: /perm path set <path>", Self::handle_perm_path_set);
        Self::register_command("/perm load", "Loads permissions.xml (current path)", Self::handle_perm_load);
        Self::register_command("/perm load from", "Loads permissions.xml from path. Usage: /perm load from <path>", Self::handle_perm_load_from);
        Self::register_command("/perm save", "Saves permissions.xml (current path)", Self::handle_perm_save);
        Self::register_command("/perm save to", "Saves permissions.xml to path. Usage: /perm save to <path>", Self::handle_perm_save_to);
        Self::register_command("/perm reload", "Save then load (current path)", Self::handle_perm_reload);
        Self::register_command("/perm defaults", "Ensures default groups exist", Self::handle_perm_defaults);

        // Users
        Self::register_command("/perm user list", "Lists all users", Self::handle_perm_user_list);
        Self::register_command("/perm user create", "Creates user. Usage: /perm user create <uuid>", Self::handle_perm_user_create);
        Self::register_command("/perm user info", "Shows user raw nodes/groups. Usage: /perm user info <uuid>", Self::handle_perm_user_info);
        Self::register_command("/perm user node add", "Adds user node. Usage: /perm user node add <uuid> <node>", Self::handle_perm_user_node_add);
        Self::register_command("/perm user node remove", "Removes user node. Usage: /perm user node remove <uuid> <node>", Self::handle_perm_user_node_remove);
        Self::register_command("/perm user group add", "Adds user to group. Usage: /perm user group add <uuid> <group>", Self::handle_perm_user_group_add);
        Self::register_command("/perm user group remove", "Removes user from group. Usage: /perm user group remove <uuid> <group>", Self::handle_perm_user_group_remove);
        Self::register_command("/perm user effective", "Shows effective allow/deny rules. Usage: /perm user effective <uuid>", Self::handle_perm_user_effective);

        // Groups
        Self::register_command("/perm group list", "Lists all groups", Self::handle_perm_group_list);
        Self::register_command("/perm group create", "Creates group. Usage: /perm group create <name>", Self::handle_perm_group_create);
        Self::register_command("/perm group info", "Shows group nodes/parents. Usage: /perm group info <name>", Self::handle_perm_group_info);
        Self::register_command("/perm group node add", "Adds group node. Usage: /perm group node add <group> <node>", Self::handle_perm_group_node_add);
        Self::register_command("/perm group node remove", "Removes group node. Usage: /perm group node remove <group> <node>", Self::handle_perm_group_node_remove);
        Self::register_command("/perm group parent add", "Adds parent. Usage: /perm group parent add <group> <parent>", Self::handle_perm_group_parent_add);
        Self::register_command("/perm group parent remove", "Removes parent. Usage: /perm group parent remove <group> <parent>", Self::handle_perm_group_parent_remove);

        // Checks
        Self::register_command("/perm check", "Checks a node. Usage: /perm check <uuid> <node>", Self::handle_perm_check);

        // Quality-of-life aliases
        Self::register_command("/perm u", "Alias: /perm user ...", Self::handle_perm_help);
        Self::register_command("/perm g", "Alias: /perm group ...", Self::handle_perm_help);
    }

    fn pm() -> &'static PermissionManager {
        PermissionIntegration::manager()
    }

    fn handle_perm_root(args: &[String]) {
        Self::handle_perm_help(args);
    }

    fn handle_perm_help(_args: &[String]) {
        for line in [
            "Permission commands:",
            "/perm path",
            "/perm path set <path>",
            "/perm load",
            "/perm load from <path>",
            "/perm save",
            "/perm save to <path>",
            "/perm reload",
            "/perm defaults",
            "",
            "/perm user list",
            "/perm user create <uuid>",
            "/perm user info <uuid>",
            "/perm user node add <uuid> <node>",
            "/perm user node remove <uuid> <node>",
            "/perm user group add <uuid> <group>",
            "/perm user group remove <uuid> <group>",
            "/perm user effective <uuid>",
            "",
            "/perm group list",
            "/perm group create <name>",
            "/perm group info <name>",
            "/perm group node add <group> <node>",
            "/perm group node remove <group> <node>",
            "/perm group parent add <group> <parent>",
            "/perm group parent remove <group> <parent>",
            "",
            "/perm check <uuid> <node>",
            "Notes: Use '-node' to deny when adding nodes.",
        ] {
            BNL::log(line);
        }
    }

    // -------- IO / path --------

    fn handle_perm_path(_args: &[String]) {
        BNL::log(format!("permissions.xml path: {}", Self::pm().get_xml_path().display()));
    }

    fn handle_perm_path_set(args: &[String]) {
        if args.is_empty() {
            BNL::log("Usage: /perm path set <path>");
            return;
        }
        let path = args.join(" ").trim().to_string(); // allow spaces
        match Self::pm().set_xml_path(&path) {
            Ok(()) => BNL::log(format!("Set permissions.xml path to: {}", Self::pm().get_xml_path().display())),
            Err(e) => BNL::log_error(format!("Could not set the permissions.xml path: {}", e.report())),
        }
    }

    fn handle_perm_load(_args: &[String]) {
        match Self::pm().load_from_xml(None) {
            Ok(()) => BNL::log(format!("Loaded permissions from: {}", Self::pm().get_xml_path().display())),
            Err(e) => BNL::log_error(format!("Could not load permissions: {}", e.report())),
        }
    }

    fn handle_perm_load_from(args: &[String]) {
        if args.is_empty() {
            BNL::log("Usage: /perm load from <path>");
            return;
        }
        let path = args.join(" ").trim().to_string();
        let pm = Self::pm();
        match pm.load_from_xml(Some(std::path::Path::new(&path))).and_then(|()| pm.set_xml_path(&path)) {
            Ok(()) => BNL::log(format!("Loaded permissions from: {path}")),
            Err(e) => BNL::log_error(format!("Could not load permissions from {path}: {}", e.report())),
        }
    }

    fn handle_perm_save(_args: &[String]) {
        match Self::pm().save_to_xml(None) {
            Ok(()) => BNL::log(format!("Saved permissions to: {}", Self::pm().get_xml_path().display())),
            Err(e) => BNL::log_error(format!("Could not save permissions: {}", e.report())),
        }
    }

    fn handle_perm_save_to(args: &[String]) {
        if args.is_empty() {
            BNL::log("Usage: /perm save to <path>");
            return;
        }
        let path = args.join(" ").trim().to_string();
        match Self::pm().save_to_xml(Some(std::path::Path::new(&path))) {
            Ok(()) => BNL::log(format!("Saved permissions to: {path}")),
            Err(e) => BNL::log_error(format!("Could not save permissions to {path}: {}", e.report())),
        }
    }

    fn handle_perm_reload(_args: &[String]) {
        let pm = Self::pm();
        match pm.save_to_xml(None).and_then(|()| pm.load_from_xml(None)) {
            Ok(()) => BNL::log("Reloaded permissions (save -> load)."),
            Err(e) => BNL::log_error(format!("Could not reload permissions: {}", e.report())),
        }
    }

    fn handle_perm_defaults(_args: &[String]) {
        Self::pm().ensure_defaults();
        BNL::log("Ensured default permission groups.");
    }

    // -------- Users --------

    fn sorted_ci(mut values: Vec<String>) -> Vec<String> {
        values.sort_by_key(|v| v.to_lowercase());
        values
    }

    fn list_or_none(values: Vec<String>) -> String {
        if values.is_empty() { "(none)".to_string() } else { values.join(", ") }
    }

    fn handle_perm_user_list(_args: &[String]) {
        let snap = Self::pm().snapshot();
        if snap.users.is_empty() {
            BNL::log("No users.");
            return;
        }
        BNL::log(format!("Users ({}):", snap.users.len()));
        for u in Self::sorted_ci(snap.users.keys()) {
            BNL::log(format!("- {u}"));
        }
    }

    fn handle_perm_user_create(args: &[String]) {
        if args.is_empty() {
            BNL::log("Usage: /perm user create <uuid>");
            return;
        }
        let uuid = args[0].trim();
        let _ = Self::pm().get_or_create_user(uuid);
        Self::pm().save_to_xml_debounced();
        BNL::log(format!("User ensured: {uuid}"));
    }

    fn handle_perm_user_info(args: &[String]) {
        if args.is_empty() {
            BNL::log("Usage: /perm user info <uuid>");
            return;
        }
        let uuid = args[0].trim();
        let Some(user) = Self::pm().try_get_user(uuid) else {
            BNL::log(format!("User not found: {uuid}"));
            return;
        };
        BNL::log(format!("User: {}", user.uuid));
        BNL::log(format!("Groups ({}): {}", user.groups.len(), Self::list_or_none(Self::sorted_ci(user.groups.to_vec()))));
        BNL::log(format!("Nodes ({}): {}", user.nodes.len(), Self::list_or_none(Self::sorted_ci(user.nodes.to_vec()))));
    }

    fn handle_perm_user_node_add(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm user node add <uuid> <node>");
            return;
        }
        let uuid = args[0].trim();
        let node = args[1..].join(" ").trim().to_string(); // allow weird node strings
        Self::pm().add_user_node(uuid, &node);
        BNL::log(format!("Added user node: {uuid} -> {node}"));
    }

    fn handle_perm_user_node_remove(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm user node remove <uuid> <node>");
            return;
        }
        let uuid = args[0].trim();
        let node = args[1..].join(" ").trim().to_string();
        Self::pm().remove_user_node(uuid, &node);
        BNL::log(format!("Removed user node: {uuid} -> {node}"));
    }

    fn handle_perm_user_group_add(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm user group add <uuid> <group>");
            return;
        }
        let uuid = args[0].trim();
        let group = args[1..].join(" ").trim().to_string();
        Self::pm().add_user_to_group(uuid, &group);
        BNL::log(format!("Added user to group: {uuid} -> {group}"));
    }

    fn handle_perm_user_group_remove(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm user group remove <uuid> <group>");
            return;
        }
        let uuid = args[0].trim();
        let group = args[1..].join(" ").trim().to_string();
        Self::pm().remove_user_from_group(uuid, &group);
        BNL::log(format!("Removed user from group: {uuid} -> {group}"));
    }

    fn handle_perm_user_effective(args: &[String]) {
        if args.is_empty() {
            BNL::log("Usage: /perm user effective <uuid>");
            return;
        }
        let uuid = args[0].trim();
        let allowed = Self::sorted_ci(Self::pm().get_all_allowed_rules(uuid));
        let denied = Self::sorted_ci(Self::pm().get_all_denied_rules(uuid));
        BNL::log(format!("Effective rules for {uuid}:"));
        BNL::log(format!("Allowed ({}): {}", allowed.len(), Self::list_or_none(allowed)));
        BNL::log(format!("Denied ({}): {}", denied.len(), Self::list_or_none(denied)));
    }

    // -------- Groups --------

    fn handle_perm_group_list(_args: &[String]) {
        let snap = Self::pm().snapshot();
        if snap.groups.is_empty() {
            BNL::log("No groups.");
            return;
        }
        BNL::log(format!("Groups ({}):", snap.groups.len()));
        for g in Self::sorted_ci(snap.groups.keys()) {
            BNL::log(format!("- {g}"));
        }
    }

    fn handle_perm_group_create(args: &[String]) {
        if args.is_empty() {
            BNL::log("Usage: /perm group create <name>");
            return;
        }
        let name = args.join(" ").trim().to_string();
        let _ = Self::pm().get_or_create_group(&name);
        Self::pm().save_to_xml_debounced();
        BNL::log(format!("Group ensured: {name}"));
    }

    fn handle_perm_group_info(args: &[String]) {
        if args.is_empty() {
            BNL::log("Usage: /perm group info <name>");
            return;
        }
        let name = args.join(" ").trim().to_string();
        let Some(group) = Self::pm().try_get_group(&name) else {
            BNL::log(format!("Group not found: {name}"));
            return;
        };
        BNL::log(format!("Group: {}", group.name));
        BNL::log(format!("Parents ({}): {}", group.parents.len(), Self::list_or_none(Self::sorted_ci(group.parents.to_vec()))));
        BNL::log(format!("Nodes ({}): {}", group.nodes.len(), Self::list_or_none(Self::sorted_ci(group.nodes.to_vec()))));
    }

    fn handle_perm_group_node_add(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm group node add <group> <node>");
            return;
        }
        let group = args[0].trim();
        let node = args[1..].join(" ").trim().to_string();
        Self::pm().add_group_node(group, &node);
        BNL::log(format!("Added group node: {group} -> {node}"));
    }

    fn handle_perm_group_node_remove(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm group node remove <group> <node>");
            return;
        }
        let group = args[0].trim();
        let node = args[1..].join(" ").trim().to_string();
        Self::pm().remove_group_node(group, &node);
        BNL::log(format!("Removed group node: {group} -> {node}"));
    }

    fn handle_perm_group_parent_add(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm group parent add <group> <parent>");
            return;
        }
        let group = args[0].trim();
        let parent = args[1..].join(" ").trim().to_string();
        Self::pm().add_group_parent(group, &parent);
        BNL::log(format!("Added parent: {group} -> {parent}"));
    }

    fn handle_perm_group_parent_remove(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm group parent remove <group> <parent>");
            return;
        }
        let group = args[0].trim();
        let parent = args[1..].join(" ").trim().to_string();
        Self::pm().remove_group_parent(group, &parent);
        BNL::log(format!("Removed parent: {group} -> {parent}"));
    }

    // -------- Checks --------

    fn handle_perm_check(args: &[String]) {
        if args.len() < 2 {
            BNL::log("Usage: /perm check <uuid> <node>");
            return;
        }
        let uuid = args[0].trim();
        let node = args[1..].join(" ").trim().to_string();
        let has = Self::pm().has(uuid, &node);
        BNL::log(format!("Check: uuid={uuid} node={node} => {}", if has { "ALLOW" } else { "DENY" }));
    }

    // ── Reader thread ──

    pub fn start_console_listener() {
        BasisConsoleDriver::initialize();
        let spawned = std::thread::Builder::new().name("BasisConsole".into()).spawn(|| {
            while Program::is_running() {
                let Some(line) = BasisConsoleDriver::read_line() else {
                    break; // end of input: nothing left to read, don't spin on it
                };
                Self::execute(&line);
            }
        });
        if let Err(e) = spawned {
            BNL::log_error(format!("The console reader thread could not start: {e}"));
        }
    }

    /// Dispatches one typed line: the longest registered prefix wins and the rest are its
    /// arguments. Returns whether a command matched.
    pub fn execute(line: &str) -> bool {
        let input = line.trim();
        if input.is_empty() {
            return true;
        }
        let parts: Vec<String> = input.split(' ').filter(|p| !p.is_empty()).map(str::to_string).collect();

        // Try to match the longest possible command
        for i in (1..=parts.len()).rev() {
            let potential_command = parts[..i].join(" ").to_lowercase();
            if let Some(command) = Self::find_command(&potential_command) {
                let args = &parts[i..];
                let handler = command.handler.clone();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(args))).is_err() {
                    BNL::log(format!("Error executing command '{potential_command}'."));
                }
                return true;
            }
        }
        BNL::log("Unknown command. Type /help for available commands.");
        false
    }

    // ── Built-in commands ──

    pub fn handle_show_players(_args: &[String]) {
        let peers = NetworkServer::authenticated_peers();
        let mut connected_player_names = format!("Connected Player count is {} ", peers.len());
        for entry in peers.iter() {
            if let Some(message) = BasisSavedState::get_last_player_meta_data(entry.value()) {
                connected_player_names.push_str(&format!("Player: {} UUID: {}, ", message.player_display_name, message.player_uuid));
            }
        }
        BNL::log(connected_player_names);
    }

    pub fn handle_status(_args: &[String]) {
        BNL::log("Server is running and healthy.");
    }

    pub fn handle_shutdown(_args: &[String]) {
        BNL::log("Shutting down the server...");
        Program::request_shutdown(); // Gracefully stop the server
    }

    /// Blocks until the process that launched this one via /restart has exited, so the socket
    /// bind does not race it. Returns immediately when the argument is absent, malformed, or
    /// names a process that is already gone.
    pub fn wait_for_predecessor_exit(args: &[String]) {
        let Some(argument) = args.iter().find(|a| a.to_lowercase().starts_with(Self::AWAIT_PID_ARGUMENT)) else {
            return;
        };
        let Ok(pid) = argument[Self::AWAIT_PID_ARGUMENT.len()..].parse::<i32>() else {
            return;
        };
        if pid <= 0 || !Self::process_exists(pid) {
            // Already exited, which is the common case — nothing to wait for.
            return;
        }
        BNL::log(format!("Waiting for the previous server process ({pid}) to exit..."));
        let deadline = Instant::now() + Duration::from_secs(30);
        while Self::process_exists(pid) {
            if Instant::now() >= deadline {
                BNL::log_warning(format!("Previous server process ({pid}) is still running after 30s; binding anyway."));
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn process_exists(pid: i32) -> bool {
        #[cfg(unix)]
        {
            // SAFETY: signal 0 probes for existence without delivering anything.
            let result = unsafe { libc::kill(pid, 0) };
            result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            false
        }
    }

    pub fn handle_restart(_args: &[String]) {
        let exe_path = match std::env::current_exe() {
            Ok(path) => path,
            Err(_) => {
                BNL::log_error("Cannot restart: the host process path is unavailable. Use /shutdown and start the server again.");
                return;
            }
        };

        // Launched before this process exits, so the operator keeps a running server rather than
        // being left with nothing if the relaunch fails. The replacement binds the same port, so it
        // is told to wait for this process to go away first — otherwise it races us for the socket
        // and dies on startup.
        let mut command = std::process::Command::new(&exe_path);
        command.current_dir(Configuration::base_directory());
        for argument in std::env::args().skip(1) {
            if argument.to_lowercase().starts_with(Self::AWAIT_PID_ARGUMENT) {
                continue;
            }
            command.arg(argument);
        }
        command.arg(format!("{}{}", Self::AWAIT_PID_ARGUMENT, std::process::id()));

        BNL::log("Restarting the server...");
        if let Err(e) = command.spawn() {
            BNL::log_error(format!("Restart failed to launch a replacement process, server left running: {e}"));
            return;
        }
        Program::request_shutdown();
    }

    pub fn handle_help(_args: &[String]) {
        BNL::log("Available commands:");
        for command in Self::commands() {
            if command.description.is_empty() {
                BNL::log(command.name);
            } else {
                BNL::log(format!("{} - {}", command.name, command.description));
            }
        }
    }

    pub fn handle_clear(_args: &[String]) {
        BasisConsoleDriver::clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_GATE: Mutex<()> = Mutex::new(());

    #[test]
    fn longest_prefix_wins_and_rest_are_arguments() {
        let _guard = TEST_GATE.lock();
        BasisConsoleCommands::clear_commands();
        let root_hits = Arc::new(AtomicUsize::new(0));
        let nested = Arc::new(Mutex::new(Vec::<String>::new()));
        let hits = root_hits.clone();
        BasisConsoleCommands::register_command("/thing", "root", move |_| {
            hits.fetch_add(1, Ordering::SeqCst);
        });
        let seen = nested.clone();
        BasisConsoleCommands::register_command("/thing sub", "nested", move |args| {
            *seen.lock() = args.to_vec();
        });

        assert!(BasisConsoleCommands::execute("/THING Sub  alpha beta"));
        assert_eq!(*nested.lock(), vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(root_hits.load(Ordering::SeqCst), 0);

        assert!(BasisConsoleCommands::execute("/thing other"));
        assert_eq!(root_hits.load(Ordering::SeqCst), 1);

        assert!(!BasisConsoleCommands::execute("/nothing here"));
        assert!(BasisConsoleCommands::execute("   "));
        BasisConsoleCommands::clear_commands();
    }

    #[test]
    fn re_registering_replaces_in_place() {
        let _guard = TEST_GATE.lock();
        BasisConsoleCommands::clear_commands();
        BasisConsoleCommands::register_command("/a", "first", |_| {});
        BasisConsoleCommands::register_command("/b", "second", |_| {});
        BasisConsoleCommands::register_command("/A", "replaced", |_| {});
        let commands = BasisConsoleCommands::commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].description, "replaced");
        assert_eq!(commands[1].name, "/b");
        BasisConsoleCommands::clear_commands();
    }

    #[test]
    fn predecessor_wait_returns_when_pid_is_gone_or_malformed() {
        BasisConsoleCommands::wait_for_predecessor_exit(&["--await-pid=notanumber".to_string()]);
        BasisConsoleCommands::wait_for_predecessor_exit(&["--await-pid=2147483000".to_string()]);
        BasisConsoleCommands::wait_for_predecessor_exit(&[]);
    }
}
