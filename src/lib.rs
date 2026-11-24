mod binary;

use binary::{BinaryResolver, SERVER_ID};
use zed_extension_api::{
    self as zed, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, DebugTaskDefinition,
    LanguageServerId, Result, StartDebuggingRequestArguments,
    StartDebuggingRequestArgumentsRequest, TaskTemplate, Worktree, serde_json,
    settings::LspSettings,
};

const FLUTTER_ADAPTER_NAME: &str = "Flutter";
const FLUTTER_LOCATOR_NAME: &str = "flutter";

struct FlutterDebuggerExtension {
    resolver: BinaryResolver,
}

impl zed::Extension for FlutterDebuggerExtension {
    fn new() -> Self {
        Self {
            resolver: BinaryResolver::default(),
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let binary_path = self
            .resolver
            .resolve_for_lsp(language_server_id, worktree)?;

        let mut args = vec!["lsp".to_string()];
        let mut env = worktree.shell_env();
        if let Ok(settings) = LspSettings::for_worktree(SERVER_ID, worktree)
            && let Some(binary) = settings.binary
        {
            if let Some(extra_args) = binary.arguments {
                args.extend(extra_args);
            }
            if let Some(extra_env) = binary.env {
                for (key, value) in extra_env {
                    env.retain(|(existing_key, _)| existing_key != &key);
                    env.push((key, value));
                }
            }
        }

        Ok(zed::Command {
            command: binary_path,
            args,
            env,
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let opts = LspSettings::for_worktree(SERVER_ID, worktree)
            .ok()
            .and_then(|s| s.initialization_options.or(s.settings));
        Ok(opts)
    }

    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let settings = LspSettings::for_worktree(SERVER_ID, worktree)
            .ok()
            .and_then(|s| s.settings.or(s.initialization_options));
        Ok(settings)
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary, String> {
        if adapter_name != FLUTTER_ADAPTER_NAME {
            return Err(format!("Unsupported debug adapter: {adapter_name}"));
        }

        let binary_path = self
            .resolver
            .resolve_for_dap(user_provided_debug_adapter_path, worktree)?;

        let parsed: serde_json::Value = serde_json::from_str(&config.config)
            .map_err(|err| format!("Invalid Flutter debug configuration JSON: {err}"))?;

        let request = parse_request_kind(&parsed)?;

        Ok(DebugAdapterBinary {
            command: Some(binary_path),
            arguments: vec!["dap".to_string()],
            envs: worktree.shell_env(),
            cwd: Some(worktree.root_path()),
            connection: None,
            request_args: StartDebuggingRequestArguments {
                configuration: config.config,
                request,
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        adapter_name: String,
        config: serde_json::Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest, String> {
        if adapter_name != FLUTTER_ADAPTER_NAME {
            return Err(format!("Unsupported debug adapter: {adapter_name}"));
        }
        parse_request_kind(&config)
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario, String> {
        if config.adapter != FLUTTER_ADAPTER_NAME {
            return Err(format!("Unsupported debug adapter: {}", config.adapter));
        }

        let payload = match config.request {
            DebugRequest::Launch(launch) => {
                let mut map = serde_json::Map::new();
                let program = if launch.program.is_empty() {
                    "lib/main.dart".to_string()
                } else {
                    launch.program
                };
                let is_test = looks_like_test_target(&program);

                map.insert("request".to_string(), serde_json::json!("launch"));
                map.insert("program".to_string(), serde_json::json!(program));
                if is_test {
                    map.insert("isTest".to_string(), serde_json::json!(true));
                }
                if let Some(cwd) = launch.cwd {
                    map.insert("cwd".to_string(), serde_json::json!(cwd));
                }
                if !launch.args.is_empty() {
                    map.insert("args".to_string(), serde_json::json!(launch.args));
                }
                if !launch.envs.is_empty() {
                    let env_obj: serde_json::Map<String, serde_json::Value> = launch
                        .envs
                        .into_iter()
                        .map(|(k, v)| (k, serde_json::Value::String(v)))
                        .collect();
                    map.insert("env".to_string(), serde_json::Value::Object(env_obj));
                }
                map.insert("flutterMode".to_string(), serde_json::json!("debug"));
                serde_json::Value::Object(map)
            }
            DebugRequest::Attach(_attach) => serde_json::json!({
                "request": "attach"
            }),
        };

        let config_json = serde_json::to_string(&payload)
            .map_err(|err| format!("Failed to serialize Flutter debug scenario: {err}"))?;

        Ok(DebugScenario {
            adapter: FLUTTER_ADAPTER_NAME.to_string(),
            label: config.label,
            config: config_json,
            tcp_connection: None,
            build: None,
        })
    }

    fn dap_locator_create_scenario(
        &mut self,
        locator_name: String,
        build_task: TaskTemplate,
        resolved_label: String,
        debug_adapter_name: String,
    ) -> Option<DebugScenario> {
        if locator_name != FLUTTER_LOCATOR_NAME || debug_adapter_name != FLUTTER_ADAPTER_NAME {
            return None;
        }

        let cmd = build_task.command.trim();
        let is_flutter =
            cmd == "flutter" || cmd.ends_with("/flutter") || cmd.ends_with("\\flutter.bat");
        let is_dart = cmd == "dart" || cmd.ends_with("/dart") || cmd.ends_with("\\dart.bat");
        let is_ctl = cmd == "flutter-debugger-server"
            || cmd.ends_with("/flutter-debugger-server")
            || cmd.ends_with("\\flutter-debugger-server.exe");

        if !is_flutter && !is_dart && !is_ctl {
            return None;
        }

        let (is_test_subcmd, arg_slice): (bool, &[String]) = if is_ctl {
            if build_task.args.first().map(String::as_str) == Some("ctl")
                && build_task.args.get(1).map(String::as_str) == Some("run")
            {
                (false, &build_task.args[2..])
            } else {
                return None;
            }
        } else {
            let subcommand = build_task.args.first().map(String::as_str)?;
            if subcommand != "run" && subcommand != "test" {
                return None;
            }
            (subcommand == "test", &build_task.args[1..])
        };

        let mut program = if is_test_subcmd {
            "test".to_string()
        } else {
            "lib/main.dart".to_string()
        };
        let mut device_id: Option<String> = None;
        let mut flavor: Option<String> = None;
        let mut dart_define_from_file: Option<String> = None;
        let mut flutter_mode = "debug".to_string();
        let mut explicit_cwd = build_task.cwd;
        let mut tool_args: Vec<String> = Vec::new();

        let mut iter = arg_slice.iter().peekable();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "-p" | "--project" if is_ctl => {
                    if let Some(proj_dir) = iter.next() {
                        explicit_cwd = Some(proj_dir.clone());
                    }
                }
                value if is_ctl && value.starts_with("--project=") => {
                    explicit_cwd = Some(value.trim_start_matches("--project=").to_string());
                }
                "-t" | "--target" => {
                    if let Some(target) = iter.next() {
                        program = target.clone();
                    }
                }
                "-d" | "--device-id" | "--device" => {
                    if let Some(dev) = iter.next() {
                        device_id = Some(dev.clone());
                    }
                }
                "--flavor" => {
                    if let Some(flav) = iter.next() {
                        flavor = Some(flav.clone());
                    }
                }
                "--dart-define-from-file" => {
                    if let Some(file) = iter.next() {
                        dart_define_from_file = Some(file.clone());
                    }
                }
                "-m" | "--mode" => {
                    if let Some(m) = iter.next() {
                        flutter_mode = m.clone();
                    }
                }
                "--profile" => flutter_mode = "profile".to_string(),
                "--release" => flutter_mode = "release".to_string(),
                "--debug" => flutter_mode = "debug".to_string(),
                "--" if is_ctl => {
                    // Remaining args after `--` are forwarded as Flutter toolArgs
                    for extra in iter.by_ref() {
                        tool_args.push(extra.clone());
                    }
                }
                value if value.starts_with("--dart-define-from-file=") => {
                    dart_define_from_file = Some(
                        value
                            .trim_start_matches("--dart-define-from-file=")
                            .to_string(),
                    );
                }
                value if !value.starts_with('-') && value.ends_with(".dart") => {
                    program = value.to_string();
                }
                other => {
                    tool_args.push(other.to_string());
                }
            }
        }

        let is_test = is_test_subcmd || looks_like_test_target(&program);

        let mut map = serde_json::Map::new();
        map.insert("request".to_string(), serde_json::json!("launch"));
        map.insert("program".to_string(), serde_json::json!(program));
        map.insert("flutterMode".to_string(), serde_json::json!(flutter_mode));
        if is_test {
            map.insert("isTest".to_string(), serde_json::json!(true));
        }
        if let Some(cwd) = explicit_cwd {
            map.insert("cwd".to_string(), serde_json::json!(cwd));
        }
        if let Some(dev) = device_id {
            map.insert("deviceId".to_string(), serde_json::json!(dev));
        }
        if let Some(flav) = flavor {
            map.insert("flavor".to_string(), serde_json::json!(flav));
        }
        if let Some(define_file) = dart_define_from_file {
            map.insert(
                "dartDefineFromFile".to_string(),
                serde_json::json!(define_file),
            );
        }
        if !tool_args.is_empty() {
            map.insert("toolArgs".to_string(), serde_json::json!(tool_args));
        }
        if !build_task.env.is_empty() {
            let env_obj: serde_json::Map<String, serde_json::Value> = build_task
                .env
                .into_iter()
                .map(|(k, v)| (k, serde_json::Value::String(v)))
                .collect();
            map.insert("env".to_string(), serde_json::Value::Object(env_obj));
        }

        let config_json = serde_json::to_string(&serde_json::Value::Object(map)).ok()?;

        Some(DebugScenario {
            adapter: FLUTTER_ADAPTER_NAME.to_string(),
            label: resolved_label,
            config: config_json,
            tcp_connection: None,
            build: None,
        })
    }
}

fn looks_like_test_target(program: &str) -> bool {
    let norm = program.replace('\\', "/");
    norm == "test"
        || norm == "integration_test"
        || norm.starts_with("test/")
        || norm.starts_with("integration_test/")
        || norm.starts_with("test_driver/")
        || norm.contains("/test/")
        || norm.contains("/integration_test/")
        || norm.ends_with("_test.dart")
}

fn parse_request_kind(
    config: &serde_json::Value,
) -> Result<StartDebuggingRequestArgumentsRequest, String> {
    match config.get("request").and_then(serde_json::Value::as_str) {
        Some("attach") => Ok(StartDebuggingRequestArgumentsRequest::Attach),
        Some("launch") | None => Ok(StartDebuggingRequestArgumentsRequest::Launch),
        Some(other) => Err(format!(
            "Unsupported Flutter debug request kind `{other}` (expected `launch` or `attach`)"
        )),
    }
}

zed::register_extension!(FlutterDebuggerExtension);
