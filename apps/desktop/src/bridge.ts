import { invoke, isTauri } from '@tauri-apps/api/core';
import { parseSystemStatus, type SystemStatus } from '@helmmeet/contracts';
export const desktop = isTauri();
export async function getSystemStatus(): Promise<SystemStatus> {
  if (!desktop) return { protocol_version: 1, runtime: 'browser', capture_available: false, asr_available: false, analysis_available: false };
  return parseSystemStatus(await invoke<unknown>('get_system_status'));
}
export async function call<T>(operation: Record<string, unknown>): Promise<T> {
  if (!desktop) throw new Error('请使用 npm run desktop:dev 或打包后的 HelmMeet 应用；浏览器预览不采集音频。');
  return invoke<T>('dispatch', { operation });
}
export interface Meeting { id: string; title: string; state: string; duration_ms: number; created_at: string; last_error: string | null }
export interface Segment { id: string; start_ms: number; end_ms: number; revision: number; text: string; state: string }
export interface Evidence { segment_id: string; revision: number }
export interface Fact { text: string; kind: string; evidence: Evidence[] }
export interface Action { id: string; version: number; task: string; owner_text: string | null; due_text: string | null; confirmation_state: string; execution_state: string }
export interface Artifact { id: string; type: string; version: number; status: string; content: { summary?: Fact[]; actions?: { task: string; owner_text: string | null; due_text: string | null; evidence: Evidence[] }[]; manual_text?: string }; coverage: { to_ms: number; incomplete: boolean; question?: string; model?: string; profile_id?: string; profile_name?: string; config_version?: number } }
export interface Detail extends Meeting { version: number; consent_version: number; model_profile: ModelProfile | null; sources: { source: string; state: string; device: number; gap_start_ms: number | null; reason: string | null }[]; context: { goal?: string; agenda?: string; participants?: string; terms?: string }; proposals: { id: string; action_id: string; changes: { task: string; owner_text: string | null; due_text: string | null } }[]; segments: Segment[]; artifacts: Artifact[]; actions: Action[]; notes: { id: string; time_ms: number; text: string }[]; jobs: { id: string; type: string; state: string; error: string | null }[]; analysis_mode: string; auto_summary_minutes: number; assistant_id: string; source_mask: number; transient: { asr_state?: string; partial?: string; asr_through_ms?: number; last_inference_ms?: number; levels?: Record<string, number> }; coverage: { kind: string; start_ms: number; end_ms: number; state: string; reason: string | null }[] }
export interface Settings { startup_warning: string | null; model_file: string | null; asr_configured: boolean; language: string }
export interface Template { id: string; name: string; instructions: string; version: number }

export interface ModelProfile { profile_id: string; config_version: number; display_name: string; base_url: string; model: string; context_tokens: number; max_output_tokens: number; timeout_seconds: number; has_api_key: boolean; credential_valid: boolean; archived: boolean; is_local: boolean; latest_version: number; bound_meetings: number; last_test: { tested_at: string; ok: boolean } | null }
export interface ModelDirectory { profiles: ModelProfile[]; default_profile_id: string | null }
