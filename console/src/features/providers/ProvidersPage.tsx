import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'

import { api } from '../../api/client'
import { ProviderForm } from './ProviderForm'
import type { Provider } from '../../api/types'

export function ProvidersPage() {
  const client = useQueryClient()
  const providers = useQuery({ queryKey: ['providers'], queryFn: api.providers })
  const [discoveries, setDiscoveries] = useState<Record<number, string[]>>({})
  const [editing, setEditing] = useState<Provider>()
  const create = useMutation({
    mutationFn: api.createProvider,
    onSuccess: () => client.invalidateQueries({ queryKey: ['providers'] }),
  })
  const update = useMutation({
    mutationFn: ({ id, value }: { id: number; value: Parameters<typeof api.updateProvider>[1] }) => api.updateProvider(id, value),
    onSuccess: () => { setEditing(undefined); client.invalidateQueries({ queryKey: ['providers'] }) },
  })
  const remove = useMutation({
    mutationFn: api.deleteProvider,
    onSuccess: () => client.invalidateQueries({ queryKey: ['providers'] }),
  })
  const discover = useMutation({
    mutationFn: api.discoverModels,
    onSuccess: (result, id) => setDiscoveries((current) => ({ ...current, [id]: result.models })),
  })
  const save = useMutation({
    mutationFn: ({ id, model }: { id: number; model: string }) => api.saveModels(id, [{
      model_id: model, display_name: model, enabled: true, sort_order: 0,
      reasoning_enabled: false, extra_params: {},
    }]),
    onSuccess: () => client.invalidateQueries({ queryKey: ['models'] }),
  })

  return (
    <section className="grid gap-8">
      <div><h1>Provider</h1><p>配置端点并从 Provider 拉取可用模型。</p></div>
      <ProviderForm key={editing?.provider_id ?? 'new'} provider={editing} onSave={(value) => editing
        ? update.mutate({ id: editing.provider_id, value })
        : create.mutate(value)} busy={create.isPending || update.isPending} />
      <div className="grid gap-4">
        {providers.data?.map((provider) => (
          <article key={provider.provider_id} className="rounded-xl border border-zinc-800 bg-zinc-900 p-5">
            <div className="flex items-start justify-between gap-4">
              <div><h2>{provider.name}</h2><p>{provider.provider_type} · {provider.base_url}</p></div>
              <div className="flex gap-2"><button onClick={() => setEditing(provider)}>编辑</button><button onClick={() => discover.mutate(provider.provider_id)}>拉取模型</button><button className="danger" onClick={() => remove.mutate(provider.provider_id)}>删除</button></div>
            </div>
            {discoveries[provider.provider_id] && (
              <div className="mt-4 grid gap-2 border-t border-zinc-800 pt-4">
                {discoveries[provider.provider_id].map((model) => (
                  <div key={model} className="flex items-center justify-between rounded-md bg-zinc-950 px-3 py-2 text-sm">
                    <code>{model}</code>
                    <button onClick={() => save.mutate({ id: provider.provider_id, model })}>添加</button>
                  </div>
                ))}
              </div>
            )}
          </article>
        ))}
      </div>
    </section>
  )
}
