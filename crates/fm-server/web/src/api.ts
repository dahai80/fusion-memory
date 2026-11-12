// #33: thin fetch client over the existing Bearer-guarded REST surface.
// API key lives in localStorage (local-first, offline deployment; HTTP only to 127.0.0.1).

const KEY_STORAGE = "fm.apiKey";

export function getApiKey(): string {
    return localStorage.getItem(KEY_STORAGE) ?? "";
}

export function setApiKey(key: string) {
    if (key) localStorage.setItem(KEY_STORAGE, key);
    else localStorage.removeItem(KEY_STORAGE);
}

export class ApiError extends Error {
    status: number;
    constructor(status: number, message: string) {
        super(message);
        this.status = status;
    }
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await fetch(path, {
        method,
        headers: {
            "Content-Type": "application/json",
            ...(getApiKey() ? { Authorization: `Bearer ${getApiKey()}` } : {}),
        },
        body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!res.ok) {
        let msg = `HTTP ${res.status}`;
        try {
            const j = await res.json();
            // JSON-RPC error frame (handle_rpc) or plain {error} (import).
            msg = j?.error?.message ?? j?.error ?? msg;
        } catch {
            /* non-JSON error body — keep HTTP status text */
        }
        throw new ApiError(res.status, String(msg));
    }
    return (await res.json()) as T;
}

export interface MemoryItemDto {
    id: string;
    interaction_id: string;
    turn_idx: number;
    session_id: string;
    tenant: string;
    namespace: string;
    namespace_key: string;
    memory_type: "Episodic" | "Semantic" | "Procedural";
    tier: "Working" | "Short" | "Long";
    content: string;
    weight: number;
    access_count: number;
    last_accessed_timestamp: number;
    created_timestamp: number;
    tombstone: boolean;
}

export interface AuditEntryDto {
    id: number;
    at: number;
    actor: string;
    action: string;
    target_id: string;
    detail: string;
}

export interface MergeEntryDto {
    id: number;
    at: number;
    source_id: string;
    target_id: string;
    reason: string;
}

export interface ImportReportDto {
    imported: number;
    skipped_archive: number;
    skipped_empty: number;
    failed: number;
}

export interface ListParams {
    offset: number;
    limit: number;
    memory_type?: string;
    namespace?: string;
    namespace_key?: string;
    include_tombstoned?: boolean;
}

export interface ProvenanceDto {
    id: string;
    sources: string[];
    confidence: number | null;
    cited_by: string[];
}

export const api = {
    list: (p: ListParams) => call<MemoryItemDto[]>("POST", "/v1/memory/list", p),
    auditLog: (limit: number) => call<AuditEntryDto[]>("POST", "/v1/memory/audit_log", { limit }),
    merges: () => call<MergeEntryDto[]>("POST", "/v1/memory/merges", {}),
    unmerge: (id: number) => call<{ unmerged: boolean }>("POST", "/v1/memory/unmerge", { id, confirm: true }),
    reconcile: () => call<{ reconciled: number }>("POST", "/v1/memory/reconcile", { confirm: true }),
    importStudio: (source: string) =>
        call<ImportReportDto>("POST", "/v1/memory/import", { source: source || undefined }),
    count: () => call<{ count: number }>("POST", "/v1/memory/count", {}),
    provenance: (id: string) => call<ProvenanceDto>("POST", "/v1/memory/provenance", { id }),
};
