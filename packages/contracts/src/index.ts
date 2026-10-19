export const PROTOCOL_VERSION = 1;

export type MeetingState = 'ready' | 'recording' | 'paused' | 'interrupted' |
  'finalizing' | 'completed' | 'needs_attention' | 'deleting';
export type AnalysisMode = 'transcript_only' | 'local' | 'remote';
export type AudioSource = 'microphone' | 'system';
export interface EvidenceRef { segment_id: string; revision: number }
export interface RecordingRange { start_ms: number; end_ms: number }
export interface SystemStatus {
  protocol_version: 1;
  runtime: 'tauri' | 'browser';
  capture_available: boolean;
  asr_available: boolean;
  analysis_available: boolean;
}
export interface EventEnvelope<T> {
  protocol_version: 1;
  meeting_id: string;
  event_id: string;
  sequence: number;
  emitted_at: string;
  type: string;
  payload: T;
}
export interface CommandError {
  code: 'NOT_IMPLEMENTED' | 'INVALID_STATE' | 'PERMISSION_DENIED' |
    'DISK_FULL' | 'MODEL_UNAVAILABLE' | 'VERSION_CONFLICT' |
    'CONSENT_REQUIRED' | 'INVALID_EVIDENCE' | 'INTERNAL';
  message: string;
  retryable: boolean;
}

export function parseSystemStatus(value: unknown): SystemStatus {
  if (!value || typeof value !== 'object') throw new Error('Invalid system status');
  const input = value as Record<string, unknown>;
  if (input.protocol_version !== PROTOCOL_VERSION ||
      (input.runtime !== 'tauri' && input.runtime !== 'browser') ||
      typeof input.capture_available !== 'boolean' ||
      typeof input.asr_available !== 'boolean' ||
      typeof input.analysis_available !== 'boolean') {
    throw new Error('Unsupported or invalid system status');
  }
  return { protocol_version: 1, runtime: input.runtime,
    capture_available: input.capture_available, asr_available: input.asr_available,
    analysis_available: input.analysis_available };
}

/** Global Duozhou assistant. No stored model secrets are exposed by these DTOs. */
export type AssistantInputSource = 'text' | 'voice';
export type AssistantVoiceState = 'off' | 'standby' | 'testing_wake' | 'listening' | 'transcribing' | 'thinking' | 'speaking' | 'error';
export interface AssistantPreferences {
  voice_enabled: boolean;
  wake_word: string;
  silence_ms: number;
  speak_answers: boolean;
  collections: { set_id: string; version: number }[];
}
export interface AssistantEvidence {
  source_id: string;
  kind: 'meeting' | 'knowledge' | 'method';
  text: string;
  name?: string;
  meeting_id?: string;
  segment_id?: string;
  revision?: number;
  start_ms?: number;
  version?: number;
  stale?: boolean;
  assistant_playback_overlap?: boolean;
}
export interface AssistantAnswer {
  blocks: { kind: 'meeting_fact' | 'knowledge' | 'suggestion' | 'unanswered' | 'legacy'; text: string; sources: string[] }[];
  evidence: AssistantEvidence[];
  legacy?: boolean;
  coverage?: { scope: string; through_ms: number | null; transcript_returned: number; transcript_total: number };
}
