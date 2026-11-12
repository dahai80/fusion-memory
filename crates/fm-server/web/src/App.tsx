// #33: fusion-memory web console — thin client over the existing REST surface.
// Tabs: Entries (browse) / Audit log / Merges (unmerge + reconcile) / Import (agent-studio).

import { useCallback, useEffect, useState } from "react";
import {
    Database,
    FileClock,
    GitMerge,
    KeyRound,
    Link2,
    LogIn,
    RefreshCw,
    RotateCcw,
    Shield,
    Upload,
} from "lucide-react";
import { api, ApiError, getApiKey, setApiKey } from "./api";
import type { AuditEntryDto, GovernanceConfigDto, ImportReportDto, MemoryItemDto, MergeEntryDto, ProvenanceDto } from "./api";
import { Badge, Button, Card, Input, Select, Tabs, Td, Th } from "./ui";

function fmtTs(ms: number): string {
    if (!ms) return "-";
    return new Date(ms).toLocaleString();
}

function preview(s: string, n = 120): string {
    const one = s.replace(/\s+/g, " ").trim();
    return one.length > n ? one.slice(0, n) + "…" : one;
}

async function withErr<T>(fn: () => Promise<T>, onErr: (m: string) => void): Promise<T | undefined> {
    try {
        return await fn();
    } catch (e) {
        onErr(e instanceof ApiError ? `${e.status}: ${e.message}` : String(e));
        return undefined;
    }
}

function LoginGate({ onKey }: { onKey: () => void }) {
    const [key, setKey] = useState("");
    return (
        <div className="flex min-h-screen items-center justify-center bg-zinc-950 p-4">
            <Card className="w-full max-w-md p-6">
                <div className="mb-4 flex items-center gap-3">
                    <KeyRound className="h-6 w-6 text-indigo-400" />
                    <h1 className="text-lg font-semibold text-zinc-100">fusion-memory console</h1>
                </div>
                <p className="mb-4 text-sm text-zinc-400">
                    Bearer API key (FUSION_MEMORY_API_KEY). Stored locally; all requests go to this
                    server only.
                </p>
                <div className="flex gap-2">
                    <Input type="password" value={key} onChange={setKey} placeholder="API key" className="flex-1" />
                    <Button
                        onClick={() => {
                            setApiKey(key.trim());
                            onKey();
                        }}
                    >
                        <LogIn className="h-4 w-4" /> Sign in
                    </Button>
                </div>
            </Card>
        </div>
    );
}

function EntriesTab() {
    const [items, setItems] = useState<MemoryItemDto[]>([]);
    const [offset, setOffset] = useState(0);
    const [total, setTotal] = useState<number | null>(null);
    const [mtype, setMtype] = useState("");
    const [ns, setNs] = useState("");
    const [nsKey, setNsKey] = useState("");
    const [tomb, setTomb] = useState(false);
    const [err, setErr] = useState("");
    const [busy, setBusy] = useState(false);
    const limit = 50;

    const load = useCallback(async (off: number) => {
        setBusy(true);
        setErr("");
        const rows = await withErr(
            () =>
                api.list({
                    offset: off,
                    limit,
                    memory_type: mtype || undefined,
                    namespace: ns,
                    namespace_key: nsKey,
                    include_tombstoned: tomb,
                }),
            setErr,
        );
        setBusy(false);
        if (rows) {
            setItems(rows);
            setOffset(off);
            if (rows.length < limit) setTotal(off + rows.length);
            else setTotal(null);
        }
    }, [mtype, ns, nsKey, tomb]);

    useEffect(() => {
        void load(0);
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [mtype, ns, nsKey, tomb]);

    return (
        <div className="space-y-4">
            <div className="flex flex-wrap items-center gap-2">
                <Select
                    value={mtype}
                    onChange={setMtype}
                    options={[
                        { value: "", label: "all types" },
                        { value: "Episodic", label: "Episodic" },
                        { value: "Semantic", label: "Semantic" },
                        { value: "Procedural", label: "Procedural" },
                    ]}
                />
                <Select
                    value={ns}
                    onChange={setNs}
                    options={[
                        { value: "", label: "all namespaces" },
                        { value: "user", label: "user" },
                        { value: "project", label: "project" },
                        { value: "local", label: "local" },
                    ]}
                />
                {ns !== "" && (
                    <Input value={nsKey} onChange={setNsKey} placeholder="namespace key" className="w-56" />
                )}
                <label className="flex items-center gap-2 text-sm text-zinc-400">
                    <input type="checkbox" checked={tomb} onChange={(e) => setTomb(e.target.checked)} />
                    include tombstoned
                </label>
                <Button variant="ghost" onClick={() => void load(offset)} disabled={busy}>
                    <RefreshCw className="h-4 w-4" /> Refresh
                </Button>
            </div>
            {err && <p className="text-sm text-red-400">{err}</p>}
            <Card>
                <table className="w-full">
                    <thead>
                        <tr>
                            <Th>id</Th>
                            <Th>type</Th>
                            <Th>tier</Th>
                            <Th>session</Th>
                            <Th>ns</Th>
                            <Th>weight</Th>
                            <Th>created</Th>
                            <Th>content</Th>
                        </tr>
                    </thead>
                    <tbody>
                        {items.map((m) => (
                            <tr key={m.id} className="border-t border-zinc-800/60 hover:bg-zinc-900/40">
                                <Td className="font-mono text-xs text-zinc-500">{m.id.slice(0, 10)}…</Td>
                                <Td>
                                    <Badge
                                        tone={
                                            m.memory_type === "Semantic"
                                                ? "indigo"
                                                : m.memory_type === "Procedural"
                                                  ? "emerald"
                                                  : "zinc"
                                        }
                                    >
                                        {m.memory_type}
                                    </Badge>
                                </Td>
                                <Td>{m.tier}</Td>
                                <Td className="font-mono text-xs">{m.session_id}</Td>
                                <Td className="text-xs">
                                    {m.namespace ? `${m.namespace}${m.namespace_key ? `:${m.namespace_key}` : ""}` : "-"}
                                </Td>
                                <Td>{m.weight.toFixed(2)}</Td>
                                <Td className="text-xs text-zinc-500">{fmtTs(m.created_timestamp)}</Td>
                                <Td className="max-w-md">
                                    {preview(m.content)}
                                    {m.tombstone && (
                                        <Badge tone="red">tombstoned</Badge>
                                    )}
                                </Td>
                            </tr>
                        ))}
                        {items.length === 0 && !busy && (
                            <tr>
                                <td colSpan={8} className="px-3 py-6 text-center text-sm text-zinc-500">
                                    no entries
                                </td>
                            </tr>
                        )}
                    </tbody>
                </table>
            </Card>
            <div className="flex items-center gap-2 text-sm text-zinc-400">
                <Button variant="ghost" onClick={() => void load(Math.max(0, offset - limit))} disabled={offset === 0 || busy}>
                    Prev
                </Button>
                <Button
                    variant="ghost"
                    onClick={() => void load(offset + limit)}
                    disabled={total !== null || busy || items.length < limit}
                >
                    Next
                </Button>
                {total !== null && <span>{total} entries</span>}
            </div>
        </div>
    );
}

function AuditTab() {
    const [rows, setRows] = useState<AuditEntryDto[]>([]);
    const [err, setErr] = useState("");
    const [eventFilter, setEventFilter] = useState("all");
    const load = useCallback(async () => {
        setErr("");
        const r = await withErr(() => api.auditLog(200), setErr);
        if (r) setRows(r);
    }, []);
    useEffect(() => {
        void load();
    }, [load]);
    const shown = eventFilter === "all" ? rows : rows.filter((x) => x.action.startsWith(eventFilter));
    return (
        <div className="space-y-4">
            <div className="flex flex-wrap items-center gap-2">
                <Button variant="ghost" onClick={() => void load()}>
                    <RefreshCw className="h-4 w-4" /> Refresh
                </Button>
                <span className="text-sm text-zinc-500">{shown.length} shown / {rows.length} latest (max 200)</span>
                <Select
                    value={eventFilter}
                    onChange={setEventFilter}
                    className="w-56"
                    options={[
                        { value: "all", label: "all events" },
                        { value: "trust/", label: "trust events (poison audit)" },
                        { value: "trust/injection-blocked", label: "trust/injection-blocked" },
                        { value: "trust/injection-skipped", label: "trust/injection-skipped" },
                        { value: "merge", label: "merge events" },
                        { value: "cluster", label: "cluster events" },
                    ]}
                />
            </div>
            {err && <p className="text-sm text-red-400">{err}</p>}
            <Card>
                <table className="w-full">
                    <thead>
                        <tr>
                            <Th>#</Th>
                            <Th>at</Th>
                            <Th>actor</Th>
                            <Th>action</Th>
                            <Th>target</Th>
                            <Th>detail</Th>
                        </tr>
                    </thead>
                    <tbody>
                        {rows.map((r) => (
                            <tr key={r.id} className="border-t border-zinc-800/60 hover:bg-zinc-900/40">
                                <Td className="font-mono text-xs text-zinc-500">{r.id}</Td>
                                <Td className="text-xs text-zinc-500">{fmtTs(r.at)}</Td>
                                <Td className="font-mono text-xs">{r.actor}</Td>
                                <Td>
                                    <Badge tone={r.action === "delete" ? "red" : "indigo"}>{r.action}</Badge>
                                </Td>
                                <Td className="font-mono text-xs">{r.target_id}</Td>
                                <Td className="max-w-lg text-xs">{preview(r.detail, 160)}</Td>
                            </tr>
                        ))}
                        {rows.length === 0 && (
                            <tr>
                                <td colSpan={6} className="px-3 py-6 text-center text-sm text-zinc-500">
                                    audit log empty
                                </td>
                            </tr>
                        )}
                    </tbody>
                </table>
            </Card>
        </div>
    );
}

function MergesTab() {
    const [rows, setRows] = useState<MergeEntryDto[]>([]);
    const [err, setErr] = useState("");
    const [msg, setMsg] = useState("");
    const [busy, setBusy] = useState(false);
    const load = useCallback(async () => {
        setErr("");
        const r = await withErr(() => api.merges(), setErr);
        if (r) setRows(r);
    }, []);
    useEffect(() => {
        void load();
    }, [load]);
    return (
        <div className="space-y-4">
            <div className="flex flex-wrap items-center gap-2">
                <Button variant="ghost" onClick={() => void load()}>
                    <RefreshCw className="h-4 w-4" /> Refresh
                </Button>
                <Button
                    variant="destructive"
                    disabled={busy}
                    onClick={async () => {
                        setBusy(true);
                        setErr("");
                        setMsg("");
                        const r = await withErr(() => api.reconcile(), setErr);
                        setBusy(false);
                        if (r) setMsg(`reconciled ${r.reconciled} rows (report in server log)`);
                    }}
                >
                    <RotateCcw className="h-4 w-4" /> Reconcile stores
                </Button>
                <span className="text-xs text-zinc-500">
                    reconcile: SQLite ↔ vector store diff, dangling report, tombstone physical delete
                </span>
            </div>
            {err && <p className="text-sm text-red-400">{err}</p>}
            {msg && <p className="text-sm text-emerald-400">{msg}</p>}
            <Card>
                <table className="w-full">
                    <thead>
                        <tr>
                            <Th>#</Th>
                            <Th>at</Th>
                            <Th>source</Th>
                            <Th>target</Th>
                            <Th>reason</Th>
                            <Th></Th>
                        </tr>
                    </thead>
                    <tbody>
                        {rows.map((r) => (
                            <tr key={r.id} className="border-t border-zinc-800/60 hover:bg-zinc-900/40">
                                <Td className="font-mono text-xs text-zinc-500">{r.id}</Td>
                                <Td className="text-xs text-zinc-500">{fmtTs(r.at)}</Td>
                                <Td className="font-mono text-xs">{r.source_id}</Td>
                                <Td className="font-mono text-xs">{r.target_id}</Td>
                                <Td className="text-xs">{r.reason}</Td>
                                <Td>
                                    <Button
                                        variant="destructive"
                                        disabled={busy}
                                        onClick={async () => {
                                            setBusy(true);
                                            setErr("");
                                            setMsg("");
                                            const res = await withErr(() => api.unmerge(r.id), setErr);
                                            setBusy(false);
                                            if (res) setMsg(res.unmerged ? `merge #${r.id} reverted` : `merge #${r.id} not found`);
                                            await load();
                                        }}
                                    >
                                        <GitMerge className="h-4 w-4" /> Unmerge
                                    </Button>
                                </Td>
                            </tr>
                        ))}
                        {rows.length === 0 && (
                            <tr>
                                <td colSpan={6} className="px-3 py-6 text-center text-sm text-zinc-500">
                                    no merges recorded
                                </td>
                            </tr>
                        )}
                    </tbody>
                </table>
            </Card>
        </div>
    );
}

function ImportTab() {
    const [source, setSource] = useState("");
    const [busy, setBusy] = useState(false);
    const [err, setErr] = useState("");
    const [report, setReport] = useState<ImportReportDto | null>(null);
    return (
        <div className="max-w-2xl space-y-4">
            <Card className="p-5">
                <h2 className="mb-2 flex items-center gap-2 text-sm font-semibold text-zinc-200">
                    <Upload className="h-4 w-4 text-indigo-400" /> Import agent-studio history
                </h2>
                <p className="mb-4 text-sm text-zinc-400">
                    Reads <code className="rounded bg-zinc-800 px-1">~/.fusion-agent-studio/memory.db</code>{" "}
                    by default (short_term/long_term tiers only; archive skipped). Embeds with the
                    server embedder (same one the running service uses).
                </p>
                <div className="flex flex-wrap items-center gap-2">
                    <Input value={source} onChange={setSource} placeholder="source db path (optional)" className="flex-1" />
                    <Button
                        disabled={busy}
                        onClick={async () => {
                            setBusy(true);
                            setErr("");
                            setReport(null);
                            const r = await withErr(() => api.importStudio(source.trim()), setErr);
                            setBusy(false);
                            if (r) setReport(r);
                        }}
                    >
                        <Upload className="h-4 w-4" /> {busy ? "Importing…" : "Run import"}
                    </Button>
                </div>
                {err && <p className="mt-3 text-sm text-red-400">{err}</p>}
                {report && (
                    <div className="mt-4 flex gap-3">
                        <Badge tone="emerald">imported {report.imported}</Badge>
                        <Badge tone="amber">archive skipped {report.skipped_archive}</Badge>
                        <Badge tone="amber">empty skipped {report.skipped_empty}</Badge>
                        <Badge tone="red">failed {report.failed}</Badge>
                    </div>
                )}
            </Card>
        </div>
    );
}

function ProvenanceTab() {
    const [id, setId] = useState("");
    const [rep, setRep] = useState<ProvenanceDto | null>(null);
    const [err, setErr] = useState("");
    const [busy, setBusy] = useState(false);
    return (
        <div className="max-w-2xl space-y-4">
            <Card className="p-5">
                <h2 className="mb-2 flex items-center gap-2 text-sm font-semibold text-zinc-200">
                    <Link2 className="h-4 w-4 text-indigo-400" /> Provenance query
                </h2>
                <p className="mb-4 text-sm text-zinc-400">
                    Lineage of one memory: consolidation sources (episodes / merged-away ids),
                    confidence, and reverse citations. Poisoning-governance surface (M6).
                </p>
                <div className="flex flex-wrap items-center gap-2">
                    <Input value={id} onChange={setId} placeholder="memory id" className="flex-1" />
                    <Button
                        disabled={busy || id.trim() === ""}
                        onClick={async () => {
                            setBusy(true);
                            setErr("");
                            setRep(null);
                            const r = await withErr(() => api.provenance(id.trim()), setErr);
                            setBusy(false);
                            if (r) setRep(r);
                        }}
                    >
                        <Link2 className="h-4 w-4" /> Trace
                    </Button>
                </div>
                {err && <p className="mt-3 text-sm text-red-400">{err}</p>}
                {rep && (
                    <div className="mt-4 space-y-3 text-sm">
                        <div className="flex flex-wrap items-center gap-2">
                            <Badge tone="indigo">{rep.id}</Badge>
                            {rep.confidence !== null && (
                                <Badge tone={rep.confidence >= 0.75 ? "emerald" : "amber"}>
                                    confidence {rep.confidence.toFixed(2)}
                                </Badge>
                            )}
                            {rep.confidence === null && <Badge tone="zinc">direct commit</Badge>}
                        </div>
                        <div>
                            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-zinc-500">
                                sources ({rep.sources.length})
                            </p>
                            {rep.sources.length === 0 ? (
                                <p className="text-zinc-500">none</p>
                            ) : (
                                <ul className="space-y-1 font-mono text-xs text-zinc-300">
                                    {rep.sources.map((x) => (
                                        <li key={x}>{x}</li>
                                    ))}
                                </ul>
                            )}
                        </div>
                        <div>
                            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-zinc-500">
                                cited by ({rep.cited_by.length})
                            </p>
                            {rep.cited_by.length === 0 ? (
                                <p className="text-zinc-500">none</p>
                            ) : (
                                <ul className="space-y-1 font-mono text-xs text-zinc-300">
                                    {rep.cited_by.map((x) => (
                                        <li key={x}>{x}</li>
                                    ))}
                                </ul>
                            )}
                        </div>
                    </div>
                )}
            </Card>
        </div>
    );
}

function GovernanceTab() {
    const [cfg, setCfg] = useState<GovernanceConfigDto | null>(null);
    const [err, setErr] = useState("");
    const load = useCallback(async () => {
        setErr("");
        const r = await withErr(() => api.governanceConfig(), setErr);
        if (r) setCfg(r);
    }, []);
    useEffect(() => {
        void load();
    }, [load]);
    return (
        <div className="max-w-2xl space-y-4">
            <Card className="p-5">
                <h2 className="mb-2 flex items-center gap-2 text-sm font-semibold text-zinc-200">
                    <Shield className="h-4 w-4 text-indigo-400" /> Scoring and forgetting parameters
                </h2>
                <p className="mb-4 text-sm text-zinc-400">
                    Read-only view of the tuned retrieval/fusion and forgetting-curve constants
                    (v1.9.0 governance surface). Poisoning lineage lives in the Provenance tab;
                    poisoning audit events in the Audit log trust filter.
                </p>
                {err && <p className="text-sm text-red-400">{err}</p>}
                {cfg && (
                    <div className="space-y-4 text-sm">
                        <div>
                            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-zinc-500">{cfg.fusion.description}</p>
                            <div className="flex flex-wrap gap-2">
                                <Badge tone="indigo">cosine {cfg.fusion.cosine}</Badge>
                                <Badge tone="indigo">bm25 {cfg.fusion.bm25}</Badge>
                                <Badge tone="indigo">graph {cfg.fusion.graph}</Badge>
                                <Badge tone="indigo">freshness {cfg.fusion.freshness}</Badge>
                            </div>
                        </div>
                        <div>
                            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-zinc-500">{cfg.validity.description}</p>
                            <div className="flex flex-wrap gap-2">
                                <Badge tone="amber">expired factor {cfg.validity.expired_factor}</Badge>
                            </div>
                        </div>
                        <div>
                            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-zinc-500">{cfg.forgetting.description}</p>
                            <div className="flex flex-wrap gap-2">
                                <Badge tone="emerald">theta drop {cfg.forgetting.theta_drop}</Badge>
                                <Badge tone="emerald">theta promote {cfg.forgetting.theta_promote}</Badge>
                                <Badge tone="emerald">recall promote {cfg.forgetting.episodic_recall_promote} in {Math.round(cfg.forgetting.episodic_recall_window_secs / 86400)}d</Badge>
                            </div>
                        </div>
                        <div>
                            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-zinc-500">{cfg.three_way_ablation_arm.description}</p>
                            <div className="flex flex-wrap gap-2">
                                <Badge tone="zinc">alpha {cfg.three_way_ablation_arm.alpha_cosine}</Badge>
                                <Badge tone="zinc">beta {cfg.three_way_ablation_arm.beta_decay}</Badge>
                                <Badge tone="zinc">gamma {cfg.three_way_ablation_arm.gamma_graph}</Badge>
                                <Badge tone="zinc">graph hops {cfg.graph.hop_limit}</Badge>
                            </div>
                        </div>
                    </div>
                )}
            </Card>
        </div>
    );
}

export default function App() {
    const [authed, setAuthed] = useState(!!getApiKey());
    const [tab, setTab] = useState("entries");
    const [count, setCount] = useState<number | null>(null);
    useEffect(() => {
        if (!authed) return;
        void withErr(async () => {
            const r = await api.count();
            setCount(r.count);
            return r;
        }, () => setCount(null));
    }, [authed]);
    if (!authed) return <LoginGate onKey={() => setAuthed(true)} />;
    return (
        <div className="min-h-screen bg-zinc-950 p-6 text-zinc-200">
            <div className="mx-auto max-w-6xl space-y-5">
                <div className="flex flex-wrap items-center justify-between gap-3">
                    <div className="flex items-center gap-3">
                        <Database className="h-6 w-6 text-indigo-400" />
                        <h1 className="text-lg font-semibold text-zinc-100">fusion-memory console</h1>
                        {count !== null && <Badge tone="indigo">{count} entries</Badge>}
                    </div>
                    <Tabs
                        active={tab}
                        onSelect={setTab}
                        tabs={[
                            { id: "entries", label: "Entries", icon: <Database className="h-4 w-4" /> },
                            { id: "audit", label: "Audit log", icon: <FileClock className="h-4 w-4" /> },
                            { id: "merges", label: "Merges", icon: <GitMerge className="h-4 w-4" /> },
                            { id: "provenance", label: "Provenance", icon: <Link2 className="h-4 w-4" /> },
                            { id: "governance", label: "Governance", icon: <Shield className="h-4 w-4" /> },
                            { id: "import", label: "Import", icon: <Upload className="h-4 w-4" /> },
                        ]}
                    />
                    <Button
                        variant="ghost"
                        onClick={() => {
                            setApiKey("");
                            setAuthed(false);
                        }}
                    >
                        Sign out
                    </Button>
                </div>
                {tab === "entries" && <EntriesTab />}
                {tab === "audit" && <AuditTab />}
                {tab === "merges" && <MergesTab />}
                {tab === "provenance" && <ProvenanceTab />}
                {tab === "governance" && <GovernanceTab />}
                {tab === "import" && <ImportTab />}
            </div>
        </div>
    );
}
