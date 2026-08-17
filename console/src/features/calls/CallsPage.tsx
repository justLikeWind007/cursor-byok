import { useQuery } from '@tanstack/react-query'

import { api } from '../../api/client'
import { Link } from 'react-router-dom'

export function CallsPage() {
  const calls = useQuery({ queryKey: ['calls'], queryFn: api.calls, refetchInterval: 3000 })
  return <section><div><h1>LLM Calls</h1><p>每一行对应一次真实 Provider 请求。</p></div>
    <div className="mt-6 overflow-hidden rounded-xl border border-zinc-800">
      <table><thead><tr><th>时间</th><th>模型</th><th>状态</th><th>TTFT</th><th>耗时</th><th>Tokens</th></tr></thead>
        <tbody>{calls.data?.map((call) => <tr key={call.call_id}>
          <td><Link className="text-blue-400 hover:underline" to={`/calls/${encodeURIComponent(call.call_id)}`}>{new Date(call.created_at_ms).toLocaleString()}</Link></td><td>{call.display_name}<small>{call.model_id}</small></td><td>{call.status}</td>
          <td>{call.ttft_ms == null ? '—' : `${call.ttft_ms} ms`}</td><td>{call.duration_ms == null ? '—' : `${call.duration_ms} ms`}</td><td>{call.total_tokens ?? '—'}</td>
        </tr>)}</tbody></table>
    </div>
  </section>
}
