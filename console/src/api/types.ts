export type ProviderType = 'openai-chat' | 'openai-responses' | 'anthropic'

export interface Provider {
  provider_id: number
  name: string
  provider_type: ProviderType
  base_url: string
  has_api_key: boolean
  custom_headers: Record<string, string | null>
  created_at_ms: number
  updated_at_ms: number
}

export interface ProviderInput {
  name: string
  provider_type: ProviderType
  base_url: string
  api_key?: string
  custom_headers: Record<string, string | null>
}

export interface ProviderModel {
  model_hash: string
  provider_id: number
  model_id: string
  display_name: string
  enabled: boolean
  sort_order: number
  context_window_tokens?: number
  max_output_tokens?: number
  reasoning_enabled: boolean
  reasoning_effort?: string
  extra_params: Record<string, unknown>
  created_at_ms: number
  updated_at_ms: number
}

export interface LlmCall {
  call_id: string
  run_id: string
  conversation_id: string
  model_hash?: string
  model_id: string
  display_name: string
  provider_type: ProviderType
  status: string
  created_at_ms: number
  duration_ms?: number
  ttfb_ms?: number
  ttft_ms?: number
  input_tokens?: number
  output_tokens?: number
  total_tokens?: number
  detailed: boolean
}

export interface CallDetail {
  call: LlmCall
  request?: { headers: Record<string, string>; body: unknown; byte_count: number }
  response_chunks: { seq: number; received_offset_ms: number; data: string; byte_count: number }[]
}
