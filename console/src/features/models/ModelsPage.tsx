import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'

import { api } from '../../api/client'
import type { ProviderModel } from '../../api/types'

export function ModelsPage() {
  const client = useQueryClient()
  const models = useQuery({ queryKey: ['models'], queryFn: api.models })
  const save = useMutation({
    mutationFn: ({ model, input }: { model: ProviderModel; input: ModelEdit }) =>
      api.saveModels(model.provider_id, [{
        model_id: model.model_id, display_name: input.displayName, enabled: input.enabled, sort_order: model.sort_order,
        context_window_tokens: input.contextWindow || undefined, max_output_tokens: input.maxOutput || undefined,
        reasoning_enabled: input.reasoning, reasoning_effort: input.effort || undefined,
        extra_params: model.extra_params,
      }]),
    onSuccess: () => client.invalidateQueries({ queryKey: ['models'] }),
  })
  return <section><div><h1>Models</h1><p>Hash 是 Cursor 和其他客户端使用的稳定公开标识。</p></div>
    <div className="mt-6 overflow-hidden rounded-xl border border-zinc-800">
      <table><thead><tr><th>Hash / Provider ID</th><th>Display name</th><th>Context</th><th>Max output</th><th>Reasoning</th><th>状态</th><th></th></tr></thead>
        <tbody>{models.data?.map((model) => <ModelRow key={model.model_hash} model={model} onSave={(input) => save.mutate({ model, input })} />)}</tbody></table>
    </div>
  </section>
}

interface ModelEdit { displayName: string; enabled: boolean; contextWindow: number; maxOutput: number; reasoning: boolean; effort: string }

function ModelRow({ model, onSave }: { model: ProviderModel; onSave: (input: ModelEdit) => void }) {
  const [displayName, setDisplayName] = useState(model.display_name)
  const [contextWindow, setContextWindow] = useState(model.context_window_tokens ?? 0)
  const [maxOutput, setMaxOutput] = useState(model.max_output_tokens ?? 0)
  const [reasoning, setReasoning] = useState(model.reasoning_enabled)
  const [effort, setEffort] = useState(model.reasoning_effort ?? '')
  const value = (enabled: boolean): ModelEdit => ({ displayName, enabled, contextWindow, maxOutput, reasoning, effort })
  return <tr>
    <td><code>{model.model_hash}</code><small>{model.model_id}</small></td>
    <td><input value={displayName} onChange={(event) => setDisplayName(event.target.value)} /></td>
    <td><input type="number" value={contextWindow || ''} onChange={(event) => setContextWindow(Number(event.target.value))} /></td>
    <td><input type="number" value={maxOutput || ''} onChange={(event) => setMaxOutput(Number(event.target.value))} /></td>
    <td><div className="flex items-center gap-2"><input className="h-4 w-4" type="checkbox" checked={reasoning} onChange={(event) => setReasoning(event.target.checked)} /><input placeholder="effort" value={effort} onChange={(event) => setEffort(event.target.value)} /></div></td>
    <td>{model.enabled ? 'Enabled' : 'Disabled'}</td>
    <td><div className="flex gap-2"><button onClick={() => onSave(value(model.enabled))}>保存</button><button onClick={() => onSave(value(!model.enabled))}>{model.enabled ? '停用' : '启用'}</button></div></td>
  </tr>
}
