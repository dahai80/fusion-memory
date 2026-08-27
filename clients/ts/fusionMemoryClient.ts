/**
 * fusion-memory reference HTTP client (fusion-code vendor pattern).
 * PRD §10.2。消费方参照 fusion-code src/services/kb/fusion-kb-client.ts 风格。
 *
 * 安装: 复制本文件到消费方 src/services/memory/fusionMemoryClient.ts。
 * env (operator 配置, 默认端口避让 fusion-kb 11435):
 *   FUSION_MEMORY_BASE_URL  (默认 http://127.0.0.1:11440, 避让 fusion-kb)
 *   FUSION_MEMORY_API_KEY   (必配, 对齐 fm-server Bearer B5)
 *
 * wire: JSON-RPC 2.0 envelope {jsonrpc,method,params,id} → {result|error,id}。
 *   commit → result: string[] (turn 级 memory_id)
 *   retrieve → result: {blocks:ContextBlock[], total_tokens:number}
 *   consolidate → result: {dropped,promoted,merged,...}
 *   get → result: MemoryItem | null
 *   delete (confirm=true) → result: "deleted"
 */

export interface Turn {
    turn_idx: number
    user_message: string
    assistant_message: string
    tool_calls: ToolCall[]
}

export interface ToolCall {
    name: string
    args: unknown
    result_summary: string
}

export interface Interaction {
    id: string
    session_id: string
    turns: Turn[]
    timestamp: number
    metadata: Record<string, unknown>
}

export interface ContextBlock {
    interaction_id: string
    turns: Turn[]
    memory_type: string
    turns_text: string
    score: number
    source_entities: string[]
}

export interface FormattedContext {
    blocks: ContextBlock[]
    total_tokens: number
}

export interface ConsolidationReport {
    dropped: number
    promoted: number
    merged: number
    summarized: number
    reextracted: number
    reconciled: number
}

const DEFAULT_BASE_URL = 'http://127.0.0.1:11440'

function getBaseUrl(): string {
    return process.env.FUSION_MEMORY_BASE_URL || DEFAULT_BASE_URL
}

function getApiKey(): string {
    const key = process.env.FUSION_MEMORY_API_KEY
    if (!key) {
        throw new Error('FUSION_MEMORY_API_KEY 未配置 (fm-server Bearer 鉴权, B5)')
    }
    return key
}

interface RpcResponse<T> {
    jsonrpc: string
    result?: T
    error?: { code: number; message: string }
    id: number
}

async function rpc<T>(method: string, params: Record<string, unknown>): Promise<T> {
    const res = await fetch(`${getBaseUrl()}/v1/memory/${method}`, {
        method: 'POST',
        headers: {
            'content-type': 'application/json',
            authorization: `Bearer ${getApiKey()}`,
        },
        body: JSON.stringify({ jsonrpc: '2.0', method, params, id: 1 }),
        signal: AbortSignal.timeout(10000),
    })
    if (!res.ok) {
        throw new Error(`fusion-memory ${method} HTTP ${res.status} ${res.statusText}`)
    }
    const data = (await res.json()) as RpcResponse<T>
    if (data.error) {
        throw new Error(`fusion-memory ${method} RPC ${data.error.code}: ${data.error.message}`)
    }
    return data.result as T
}

export async function commitEpisodicMemory(
    sessionId: string,
    interaction: Interaction,
): Promise<string[]> {
    return rpc<string[]>('commit', { session_id: sessionId, interaction })
}

export async function retrieveContext(
    text: string,
    topK = 10,
    tokenBudget = 4096,
    aggregate = true,
): Promise<FormattedContext> {
    return rpc<FormattedContext>('retrieve', {
        text,
        top_k: topK,
        token_budget: tokenBudget,
        aggregate,
    })
}

export async function consolidateMemories(): Promise<ConsolidationReport> {
    return rpc<ConsolidationReport>('consolidate', {})
}

export async function getMemory(id: string): Promise<unknown> {
    return rpc<unknown>('get', { id })
}

export async function deleteMemory(id: string): Promise<string> {
    return rpc<string>('delete', { id, confirm: true })
}

export async function checkHealth(): Promise<boolean> {
    try {
        const res = await fetch(`${getBaseUrl()}/healthz`, {
            method: 'GET',
            signal: AbortSignal.timeout(2000),
        })
        return res.ok
    } catch {
        return false
    }
}
