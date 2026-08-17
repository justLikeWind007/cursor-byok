import type { CallDetail, LlmCall, Provider, ProviderInput, ProviderModel } from './types'

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    ...init,
    headers: { 'content-type': 'application/json', ...init?.headers },
  })
  if (!response.ok) {
    const error = await response.json().catch(() => ({ message: response.statusText }))
    throw new Error(error.message ?? `HTTP ${response.status}`)
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}

export const api = {
  providers: () => request<Provider[]>('/api/providers'),
  createProvider: (input: ProviderInput) =>
    request<Provider>('/api/providers', { method: 'POST', body: JSON.stringify(input) }),
  updateProvider: (id: number, input: ProviderInput) =>
    request<Provider>(`/api/providers/${id}`, { method: 'PUT', body: JSON.stringify(input) }),
  deleteProvider: (id: number) => request<void>(`/api/providers/${id}`, { method: 'DELETE' }),
  discoverModels: (id: number) =>
    request<{ models: string[] }>(`/api/providers/${id}/models/discover`, { method: 'POST' }),
  saveModels: (id: number, models: unknown[]) =>
    request<ProviderModel[]>(`/api/providers/${id}/models`, {
      method: 'POST',
      body: JSON.stringify({ models }),
    }),
  models: () => request<ProviderModel[]>('/api/models'),
  deleteModel: (hash: string) => request<void>(`/api/models/${hash}`, { method: 'DELETE' }),
  calls: () => request<LlmCall[]>('/api/llm-calls'),
  call: (id: string) => request<CallDetail>(`/api/llm-calls/${encodeURIComponent(id)}`),
  observability: () => request<{ detailed: boolean }>('/api/settings/observability'),
  setObservability: (detailed: boolean) => request<{ detailed: boolean }>('/api/settings/observability', {
    method: 'PUT', body: JSON.stringify({ detailed }),
  }),
}
