/**
 * TypeScript interfaces for ~/.claude/telemetry/
 */

export type TelemetryEventName =
  | 'tengu_binary_download_attempt'
  | 'tengu_binary_download_success'
  | 'tengu_bridge_repl_poll_error'
  | 'tengu_bridge_repl_poll_give_up'
  | 'tengu_bridge_repl_teardown'
  | 'tengu_bridge_repl_ws_closed'
  | 'tengu_claudeai_limits_status_changed'
  | 'tengu_claudeai_mcp_eligibility'
  | 'tengu_claudemd__initial_load'
  | 'tengu_config_cache_stats'
  | 'tengu_context_size'
  | 'tengu_continue'
  | 'tengu_dir_search'
  | 'tengu_exit'
  | 'tengu_feature_ok'
  | 'tengu_file_history_backup_deleted_file'
  | 'tengu_file_history_backup_file_created'
  | 'tengu_file_history_snapshot_success'
  | 'tengu_file_suggestions_git_ls_files'
  | 'tengu_grove_oauth_401_received'
  | 'tengu_init'
  | 'tengu_input_command'
  | 'tengu_mcp_cli_status'
  | 'tengu_mcp_ide_server_connection_failed'
  | 'tengu_mcp_ide_server_connection_succeeded'
  | 'tengu_mcp_server_connection_failed'
  | 'tengu_mcp_server_connection_succeeded'
  | 'tengu_mcp_server_needs_auth'
  | 'tengu_mcp_servers'
  | 'tengu_native_auto_updater_fail'
  | 'tengu_native_auto_updater_start'
  | 'tengu_native_auto_updater_success'
  | 'tengu_native_install_binary_success'
  | 'tengu_native_update_complete'
  | 'tengu_native_version_cleanup'
  | 'tengu_node_warning'
  | 'tengu_notification_method_used'
  | 'tengu_oauth_profile_fetch_success'
  | 'tengu_oauth_token_refresh_lock_acquired'
  | 'tengu_oauth_token_refresh_lock_acquiring'
  | 'tengu_oauth_token_refresh_lock_released'
  | 'tengu_oauth_token_refresh_lock_releasing'
  | 'tengu_oauth_token_refresh_lock_retry'
  | 'tengu_oauth_token_refresh_starting'
  | 'tengu_oauth_token_refresh_success'
  | 'tengu_oauth_tokens_saved'
  | 'tengu_paste_text'
  | 'tengu_plugins_loaded'
  | 'tengu_prompt_suggestion_init'
  | 'tengu_repl_hook_finished'
  | 'tengu_repo_text_file_size'
  | 'tengu_ripgrep_availability'
  | 'tengu_run_hook'
  | 'tengu_session_forked_branches_fetched'
  | 'tengu_session_resumed'
  | 'tengu_shell_set_cwd'
  | 'tengu_skill_loaded'
  | 'tengu_startup_manual_model_config'
  | 'tengu_startup_telemetry'
  | 'tengu_status_line_mount'
  | 'tengu_timer'
  | 'tengu_tip_shown'
  | 'tengu_trust_dialog_shown'
  | 'tengu_version_check_success'
  | 'tengu_version_lock_acquired'
  | 'tengu_worktree_detection'
  | 'tengu_version_check_failure';

export interface TelemetryEnv {
  platform: string;
  node_version: string;
  terminal: string;
  package_managers: string;
  runtimes: string;
  is_running_with_bun: boolean;
  is_ci: boolean;
  is_claubbit: boolean;
  is_github_action: boolean;
  is_claude_code_action: boolean;
  is_claude_ai_auth: boolean;
  version: string;
  arch: string;
  is_claude_code_remote: boolean;
  deployment_environment: string;
  is_conductor: boolean;
  version_base: string;
  /** Build timestamp added by newer native Claude Code releases. */
  build_time?: string;
  is_local_agent_mode?: boolean;
  platform_raw?: string;
  shell?: string;
  vcs?: string;
}

export interface TelemetryEventData {
  // Known `tengu_*` events, but the set grows continuously — accept any
  // string so a new event name never fails the type (autocomplete keeps
  // the known values via the union half).
  event_name: TelemetryEventName | (string & {});
  client_timestamp: string;
  model: string;
  session_id: string;
  user_type: string;
  betas: string;
  env: TelemetryEnv;
  entrypoint: string;
  is_interactive: boolean;
  client_type: string;
  additional_metadata: string;
  event_id: string;
  device_id: string;
  /** `auth` is an object (was mistyped `string`). */
  auth?: TelemetryAuth;
  email?: string;
  parent_session_id?: string;
  process?: string;
  /** Deployment the event came from, e.g. `'production'`. */
  environment?: string;
  /** Event time, distinct from {@link client_timestamp}. */
  timestamp?: string;
  /**
   * A/B experiment the session was enrolled in. `experiment_metadata` and
   * `user_attributes` are JSON encoded *as strings*, not nested objects.
   */
  experiment_id?: string;
  experiment_metadata?: string;
  variation_id?: number;
  user_attributes?: string;
}

export interface TelemetryAuth {
  organization_uuid: string;
  account_uuid: string;
}

export interface TelemetryEvent {
  event_type: 'ClaudeCodeInternalEvent';
  event_data: TelemetryEventData;
}

export interface TelemetryFile {
  sessionUuid: string;
  eventUuid: string;
  events: TelemetryEvent[];
  size: number;
}

export interface TelemetryDirectory {
  files: TelemetryFile[];
}
