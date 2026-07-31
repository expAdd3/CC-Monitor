import reasonContract from "../contracts/session-state-reasons.json";

export type SessionStateReasonContract = {
  code: string;
  label: string;
  produced: boolean;
};

export const sessionStateReasonContract =
  reasonContract satisfies SessionStateReasonContract[];

const labels = new Map(
  sessionStateReasonContract.map(({ code, label }) => [code, label]),
);

/**
 * Keep unknown persisted values visible. This is intentionally not a generic
 * fallback: a new backend reason must be added to the shared contract.
 */
export function sessionStateReasonLabel(reason: string): string {
  return labels.get(reason) ?? `未知状态原因（${reason || "空值"}）`;
}
