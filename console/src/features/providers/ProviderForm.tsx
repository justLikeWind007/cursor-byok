import { useState } from 'react'

import type { Provider, ProviderInput, ProviderType } from '../../api/types'

export function ProviderForm({ provider, onSave, busy }: { provider?: Provider; onSave: (value: ProviderInput) => void; busy: boolean }) {
  const [name, setName] = useState(provider?.name ?? '')
  const [providerType, setProviderType] = useState<ProviderType>(provider?.provider_type ?? 'openai-chat')
  const [baseUrl, setBaseUrl] = useState(provider?.base_url ?? 'https://api.openai.com/v1')
  const [apiKey, setApiKey] = useState('')
  const [headers, setHeaders] = useState(JSON.stringify(provider?.custom_headers ?? {}, null, 2))

  return (
    <form
      className="grid gap-4 rounded-xl border border-zinc-800 bg-zinc-900 p-5 md:grid-cols-2"
      onSubmit={(event) => {
        event.preventDefault()
        onSave({ name, provider_type: providerType, base_url: baseUrl, api_key: apiKey || undefined, custom_headers: JSON.parse(headers) })
      }}
    >
      <Field label="名称"><input value={name} onChange={(e) => setName(e.target.value)} required /></Field>
      <Field label="类型">
        <select value={providerType} onChange={(e) => setProviderType(e.target.value as ProviderType)}>
          <option value="openai-chat">OpenAI Chat</option>
          <option value="openai-responses">OpenAI Responses</option>
          <option value="anthropic">Anthropic</option>
        </select>
      </Field>
      <Field label="Base URL"><input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} required /></Field>
      <Field label="API Key"><input type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} /></Field>
      <Field label="Custom headers (JSON)"><textarea value={headers} onChange={(e) => setHeaders(e.target.value)} /></Field>
      <div className="md:col-span-2"><button disabled={busy}>{busy ? '保存中…' : provider ? '保存 Provider' : '添加 Provider'}</button></div>
    </form>
  )
}

function Field({ label, children }: React.PropsWithChildren<{ label: string }>) {
  return <label className="grid gap-2 text-sm text-zinc-400"><span>{label}</span>{children}</label>
}
