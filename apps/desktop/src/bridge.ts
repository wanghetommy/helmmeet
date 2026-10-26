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
export interface NewMeetingLaunch { id: string; title: string | null; meeting_type: string }
export async function getLaunchRequest(acknowledge: string | null = null): Promise<NewMeetingLaunch | null> {
  return desktop ? invoke<NewMeetingLaunch | null>('launch_request', { acknowledge }) : null;
}
export interface Meeting { id: string; title: string; state: string; duration_ms: number; created_at: string; last_error: string | null; assistant_id?: string; type_name?: string | null; parent_meeting_id?: string | null }
export interface Segment { id: string; start_ms: number; end_ms: number; revision: number; text: string; state: string }
export interface Paragraph { id: string; start_ms: number; end_ms: number; text: string; raw_text?: string; reading_blocks?: string[]; raw_reading_blocks?: string[]; segment_ids: string[]; members: {segment_id:string;revision:number}[]; open:boolean; reason:string; boundary_source:string; calibration?: { status: string; changes: { type?: string; from?: string; to?: string; reason?: string }[] } }
export interface Evidence { segment_id: string; revision: number }
export interface Fact { section?: string; text: string; kind: string; evidence: Evidence[] }
export interface Action { id: string; version: number; task: string; owner_text: string | null; due_text: string | null; confirmation_state: string; execution_state: string }
export interface Artifact { id: string; type: string; version: number; status: string; content: { summary?: Fact[]; actions?: { task: string; owner_text: string | null; due_text: string | null; evidence: Evidence[] }[]; manual_text?: string }; coverage: { to_ms: number; incomplete: boolean; assistant_playback_overlap_segments?: number; question?: string; model?: string; profile_id?: string; profile_name?: string; config_version?: number; template_version?: number; incremental?: boolean; knowledge_manifest?: { set_id: string; version: number; name: string }[]; background_excerpts?: { chunk_id: number; set_id: string; version: number; name: string; excerpt: string }[] } }
export interface AudioEnhancement { echo_cancellation: boolean; aec_applied_ms: number; aec_bypass_ms: number; aec_status: { state: string; reason: string; delay_ms: number | null }; available: boolean; mode: string; engine: string; state: string; error: string | null; processed_ms: number; total_ms: number; playable_through_ms: number; generation: number }
export interface Detail extends Meeting { capabilities?: Capabilities; capture_epoch?: string | null; knowledge?: KnowledgeBinding[]; calibration?: { enabled: boolean; mode: string; pending: number; succeeded: number; needs_review: number; failed: number; run?: { id: string; state: string; total: number; completed: number; cursor: number; error: string | null } | null }; enhancement: AudioEnhancement; template_source: string; legacy_answer_ids: string[]; summary_pending_segments: number; summary_pending_bytes?: number; schedule: SummarySchedule; template: Template | null; context_consent: boolean; conversation: ConversationTurn[]; version: number; consent_version: number; model_profile: ModelProfile | null; sources: { source: string; state: string; device: number; gap_start_ms: number | null; reason: string | null }[]; context: { goal?: string; agenda?: string; participants?: string; terms?: string }; proposals: { id: string; action_id: string; changes: { task: string; owner_text: string | null; due_text: string | null } }[]; segments: Segment[]; paragraphs?: Paragraph[]; artifacts: Artifact[]; actions: Action[]; notes: { id: string; time_ms: number; text: string }[]; jobs: { id: string; type: string; state: string; error: string | null }[]; analysis_mode: string; auto_summary_minutes: number; assistant_id: string; source_mask: number; transient: { summary_state?: string; summary_pending_chars?: number; asr_state?: string; partial?: string; asr_through_ms?: number; last_inference_ms?: number; levels?: Record<string, number> }; coverage: { kind: string; start_ms: number; end_ms: number; state: string; reason: string | null }[] }
export interface Settings { audio_enhancement_available: boolean; audio_enhancement_default: string; startup_warning: string | null; model_file: string | null; asr_configured: boolean; language: string; product?: ProductPreferences; data_directory?: string; app_version?: string }
export interface TemplatePolicy {goal:string;criteria:string;qa_rules:string}
export interface Template { id: string; name: string; instructions: string; version: number; policy?:TemplatePolicy; latest_version?:number }

export interface ModelProfile { profile_id: string; config_version: number; display_name: string; base_url: string; model: string; context_tokens: number; max_output_tokens: number; timeout_seconds: number; has_api_key: boolean; credential_valid: boolean; archived: boolean; is_local: boolean; latest_version: number; bound_meetings: number; last_test: { tested_at: string; ok: boolean } | null }
export interface ModelDirectory { profiles: ModelProfile[]; default_profile_id: string | null }
export interface TtsSettings { enabled: boolean; profile_id: string | null; config_version: number | null; model: string; speaker: string; fallback_local: boolean; credential_valid?: boolean; profile_name?: string }
export interface TranscriptSnapshot { paragraphs?: Paragraph[]; meeting_id: string; title: string; source: string; timestamps: boolean; through_ms: number; hash: string; text: string; segments: { segment_id: string; revision: number; text: string; start_ms: number }[] }
export interface SummarySchedule { migration_notice?: string | null; enabled: boolean; mode: string; version: number; failures: number; paused_reason: string | null }
export interface ConversationTurn { job_id: string; question: string; scope: string; artifact_id: string | null; state: string; error: string | null }
export interface ProductPreferences { analysis_paused: boolean; default_source_mask: number; default_input_device: number; debug_mode: boolean }
export interface Capabilities { phase: 'preparation' | 'live' | 'after'; has_audio: boolean; has_confirmed_text: boolean; has_minutes: boolean; has_actions: boolean; model_ready: boolean; can_analyze: boolean; analysis_paused: boolean }
export interface Term { id: string; canonical: string; aliases: string[]; category: string }
export interface KnowledgeSet { id: string; name: string; kind: 'documents' | 'terms'; version: number; archived: boolean; term_count: number; characters: number; bound_meetings: number }
export interface KnowledgeDraft { id?: string; name: string; kind: 'documents' | 'terms'; version?: number; archived?: boolean; content: string; terms: Term[] }
export interface KnowledgeBinding { set_id: string; version: number; name: string; kind: string; latest_version: number; archived: boolean; term_count: number }
export interface NormalizationPreview { changes: { segment_id: string; expected_revision: number; before: string; after: string; start_ms: number; hash: string }[]; conflicts: string[]; events: { id: string; segment_id: string; before_revision: number; after_revision: number; undone: boolean }[]; notice: string }
export const meetingPhase = (d: Detail) => d.capabilities?.phase ?? (d.state === 'ready' ? 'preparation' : ['recording', 'paused', 'interrupted'].includes(d.state) ? 'live' : 'after');
