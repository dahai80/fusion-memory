// #33: minimal shadcn/ui-style primitives (copy-in pattern, no runtime dep beyond tailwind).
// Slate/zinc neutrals + indigo accent, dark-first; no saturated pure colors.

import type { ReactNode } from "react";

export function Button({
    children,
    onClick,
    variant = "default",
    disabled,
    className = "",
}: {
    children: ReactNode;
    onClick?: () => void;
    variant?: "default" | "ghost" | "destructive";
    disabled?: boolean;
    className?: string;
}) {
    const base =
        "inline-flex items-center justify-center gap-2 rounded-xl text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-50 px-4 py-2 shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-indigo-500";
    const styles = {
        default: "bg-indigo-500 text-white hover:bg-indigo-400",
        ghost: "bg-zinc-800/60 text-zinc-200 hover:bg-zinc-700/60 border border-zinc-700/60",
        destructive: "bg-red-900/70 text-red-100 hover:bg-red-800/70 border border-red-800/60",
    }[variant];
    return (
        <button className={`${base} ${styles} ${className}`} onClick={onClick} disabled={disabled}>
            {children}
        </button>
    );
}

export function Input({
    value,
    onChange,
    placeholder,
    type = "text",
    className = "",
}: {
    value: string;
    onChange: (v: string) => void;
    placeholder?: string;
    type?: string;
    className?: string;
}) {
    return (
        <input
            type={type}
            value={value}
            placeholder={placeholder}
            onChange={(e) => onChange(e.target.value)}
            className={`rounded-xl border border-zinc-700/80 bg-zinc-900/60 px-3 py-2 text-sm text-zinc-100 placeholder:text-zinc-500 shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-indigo-500 ${className}`}
        />
    );
}

export function Select({
    value,
    onChange,
    options,
    className = "",
}: {
    value: string;
    onChange: (v: string) => void;
    options: { value: string; label: string }[];
    className?: string;
}) {
    return (
        <select
            value={value}
            onChange={(e) => onChange(e.target.value)}
            className={`rounded-xl border border-zinc-700/80 bg-zinc-900/60 px-3 py-2 text-sm text-zinc-100 shadow-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-indigo-500 ${className}`}
        >
            {options.map((o) => (
                <option key={o.value} value={o.value}>
                    {o.label}
                </option>
            ))}
        </select>
    );
}

export function Card({ children, className = "" }: { children: ReactNode; className?: string }) {
    return (
        <div className={`rounded-2xl border border-zinc-800 bg-zinc-900/40 shadow-md shadow-zinc-950/20 ${className}`}>
            {children}
        </div>
    );
}

export function Badge({
    children,
    tone = "zinc",
}: {
    children: ReactNode;
    tone?: "zinc" | "indigo" | "emerald" | "amber" | "red";
}) {
    const tones = {
        zinc: "bg-zinc-800 text-zinc-300 border-zinc-700",
        indigo: "bg-indigo-950/60 text-indigo-300 border-indigo-800/60",
        emerald: "bg-emerald-950/60 text-emerald-300 border-emerald-800/60",
        amber: "bg-amber-950/60 text-amber-300 border-amber-800/60",
        red: "bg-red-950/60 text-red-300 border-red-800/60",
    }[tone];
    return (
        <span className={`inline-flex items-center rounded-lg border px-2 py-0.5 text-xs font-medium ${tones}`}>
            {children}
        </span>
    );
}

export function Tabs({
    tabs,
    active,
    onSelect,
}: {
    tabs: { id: string; label: string; icon: ReactNode }[];
    active: string;
    onSelect: (id: string) => void;
}) {
    return (
        <div className="flex gap-1 rounded-2xl border border-zinc-800 bg-zinc-900/40 p-1 shadow-sm">
            {tabs.map((t) => (
                <button
                    key={t.id}
                    onClick={() => onSelect(t.id)}
                    className={`inline-flex items-center gap-2 rounded-xl px-4 py-2 text-sm font-medium transition-colors ${
                        active === t.id
                            ? "bg-indigo-500/90 text-white shadow-sm"
                            : "text-zinc-400 hover:bg-zinc-800/60 hover:text-zinc-200"
                    }`}
                >
                    {t.icon}
                    {t.label}
                </button>
            ))}
        </div>
    );
}

export function Th({ children }: { children?: ReactNode }) {
    return <th className="px-3 py-2 text-left text-xs font-medium uppercase tracking-wide text-zinc-500">{children}</th>;
}

export function Td({ children, className = "" }: { children: ReactNode; className?: string }) {
    return <td className={`px-3 py-2 text-sm text-zinc-300 ${className}`}>{children}</td>;
}
