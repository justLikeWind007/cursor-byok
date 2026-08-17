import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api } from '../../api/client'

export function ObservabilityPage() {
  const client = useQueryClient()
  const settings = useQuery({ queryKey: ['observability'], queryFn: api.observability })
  const update = useMutation({ mutationFn: api.setObservability, onSuccess: () => client.invalidateQueries({ queryKey: ['observability'] }) })
  return <section><h1>Observability</h1><p>概要始终保存；详细模式额外保存脱敏请求和流响应。</p>
    <label className="mt-6 flex max-w-xl items-center justify-between rounded-xl border border-zinc-800 bg-zinc-900 p-5">
      <span><strong>详细记录</strong><p>仅影响开启后的新调用。</p></span>
      <input className="h-5 w-5" type="checkbox" checked={settings.data?.detailed ?? false} onChange={(event) => update.mutate(event.target.checked)} />
    </label>
  </section>
}
