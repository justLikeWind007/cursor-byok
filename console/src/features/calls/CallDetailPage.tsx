import { useQuery } from '@tanstack/react-query'
import { useParams } from 'react-router-dom'

import { api } from '../../api/client'

export function CallDetailPage() {
  const { callId = '' } = useParams()
  const detail = useQuery({ queryKey: ['call', callId], queryFn: () => api.call(callId) })
  if (!detail.data) return <p>加载中…</p>
  const { call, request, response_chunks: chunks } = detail.data
  return <section className="grid gap-6">
    <div><h1>{call.display_name}</h1><p><code>{call.call_id}</code> · {call.status}</p></div>
    <div className="grid gap-3 md:grid-cols-4">
      <Metric label="TTFB" value={call.ttfb_ms} suffix="ms" /><Metric label="TTFT" value={call.ttft_ms} suffix="ms" />
      <Metric label="Duration" value={call.duration_ms} suffix="ms" /><Metric label="Total tokens" value={call.total_tokens} />
    </div>
    <Payload title="Request" value={request ?? '详细模式未记录'} />
    <Payload title="Response stream" value={chunks.length ? chunks : '详细模式未记录'} />
  </section>
}

function Metric({ label, value, suffix = '' }: { label: string; value?: number; suffix?: string }) {
  return <div className="rounded-xl border border-zinc-800 bg-zinc-900 p-4"><p>{label}</p><div className="mt-2 text-xl">{value ?? '—'} {value == null ? '' : suffix}</div></div>
}

function Payload({ title, value }: { title: string; value: unknown }) {
  return <div><h2>{title}</h2><pre className="mt-2 max-h-[32rem] overflow-auto rounded-xl border border-zinc-800 bg-black p-4 text-xs text-zinc-300">{typeof value === 'string' ? value : JSON.stringify(value, null, 2)}</pre></div>
}
