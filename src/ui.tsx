import type { ReactNode } from "react";

export function PageTitle({ title, subtitle, headingRef }: { title: string; subtitle?: string; headingRef?: (heading: HTMLHeadingElement | null) => void }) {
  return <header className="page-title"><div><h1 ref={headingRef} tabIndex={-1}>{title}</h1>{subtitle && <p>{subtitle}</p>}</div></header>;
}

export function SectionTitle({ id, title, subtitle, action }: { id: string; title: string; subtitle: string; action?: ReactNode }) {
  return <div className="section-heading"><div className="section-title"><h2 id={id}>{title}</h2><p>{subtitle}</p></div>{action && <div className="section-actions">{action}</div>}</div>;
}

export function Loading({ label = "正在读取本地监控数据…" }: { label?: string }) {
  return <div className="loading" role="status" aria-live="polite">{label}</div>;
}

export function Info({ label, value }: { label: string; value: string }) {
  return <div className="info"><span>{label}</span><strong>{value}</strong></div>;
}
