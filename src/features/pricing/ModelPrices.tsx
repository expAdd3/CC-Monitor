import { useCallback, useEffect, useRef, useState } from "react";
import { api, type ModelPrice, type SaveModelPrice } from "../../api";
import { useAsyncResource, type ActionState } from "../../lib/asyncState";
import {
  blankPrice,
  priceFieldLabels,
  priceSaveCorrection,
  validatePriceEditor,
  type PriceFieldErrors,
} from "./validation";

function PriceField({
  id,
  label,
  value,
  error,
  disabled = false,
  onChange,
  placeholder = "",
  maxLength,
}: {
  id: string;
  label: string;
  value: string;
  error?: string;
  disabled?: boolean;
  onChange: (value: string) => void;
  placeholder?: string;
  maxLength?: number;
}) {
  const errorId = error ? `${id}-error` : undefined;
  return <label className="field">
    <span>{label}</span>
    <input
      id={id}
      aria-label={label}
      value={value}
      disabled={disabled}
      placeholder={placeholder}
      maxLength={maxLength}
      aria-invalid={error ? "true" : undefined}
      aria-describedby={errorId}
      onChange={(event) => onChange(event.target.value)}
    />
    {error && <small id={errorId} className="field-error" role="alert">{error}</small>}
  </label>;
}

export function ModelPrices() {
  const loadPrices = useCallback(() => api.modelPrices(), []);
  const resource = useAsyncResource(loadPrices, "暂时无法读取定价，请重试");
  const [prices, setPrices] = useState<ModelPrice[]>([]);
  const [editing, setEditing] = useState<SaveModelPrice | null>(null);
  const [editingExisting, setEditingExisting] = useState(false);
  const [confirmation, setConfirmation] = useState<{ modelId: string } | null>(null);
  const [action, setAction] = useState<ActionState<string>>({ status: "idle" });
  const actionLock = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [fieldErrors, setFieldErrors] = useState<PriceFieldErrors>({});
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const editorRef = useRef<HTMLFieldSetElement>(null);
  const confirmationButtonRef = useRef<HTMLButtonElement>(null);
  const addButtonRef = useRef<HTMLButtonElement>(null);
  const returnFocusRef = useRef<HTMLButtonElement | null>(null);
  const restoreFocusRef = useRef<"trigger" | "fallback" | null>(null);
  const actionPending = action.status === "pending";
  const editorOpen = editing !== null;
  const confirmationOpen = confirmation !== null;

  useEffect(() => {
    if (resource.state.status === "success") setPrices(resource.state.value);
  }, [resource.state]);
  useEffect(() => {
    if (editorOpen) editorRef.current?.querySelector<HTMLInputElement>("input:not(:disabled)")?.focus();
  }, [editorOpen]);
  useEffect(() => {
    if (editorOpen && Object.keys(fieldErrors).length > 0) {
      editorRef.current?.querySelector<HTMLInputElement>('input[aria-invalid="true"]')?.focus();
    }
  }, [editorOpen, fieldErrors]);
  useEffect(() => {
    if (confirmationOpen) confirmationButtonRef.current?.focus();
  }, [confirmationOpen]);
  useEffect(() => {
    if (editorOpen || confirmationOpen || !restoreFocusRef.current) return;
    const target = restoreFocusRef.current === "trigger" && returnFocusRef.current?.isConnected
      ? returnFocusRef.current : addButtonRef.current;
    if (!target) return;
    restoreFocusRef.current = null;
    target.focus();
  }, [editorOpen, confirmationOpen, prices]);

  const update = (patch: Partial<SaveModelPrice>) => {
    setEditing((current) => current ? { ...current, ...patch } : current);
  };
  const beginEdit = (price: ModelPrice | undefined, trigger: HTMLButtonElement) => {
    if (actionLock.current || actionPending) return;
    returnFocusRef.current = trigger;
    setEditing(price ? {
      modelId: price.modelId,
      inputCostPerMillion: price.inputCostPerMillion,
      outputCostPerMillion: price.outputCostPerMillion,
      cacheWriteCostPerMillion: price.cacheWriteCostPerMillion,
      cacheReadCostPerMillion: price.cacheReadCostPerMillion,
    } : blankPrice());
    setEditingExisting(Boolean(price));
    setConfirmation(null);
    setAction({ status: "idle" });
    setError(null);
    setFieldErrors({});
    setDeleteError(null);
  };
  const closeEditor = () => {
    restoreFocusRef.current = "trigger";
    setEditing(null);
    setEditingExisting(false);
    setError(null);
    setFieldErrors({});
  };
  const save = async () => {
    if (!editing || actionLock.current || actionPending) return;
    const validation = validatePriceEditor(editing);
    if (Object.keys(validation).length > 0) {
      setFieldErrors(validation);
      setError(null);
      return;
    }
    actionLock.current = true;
    setError(null);
    setFieldErrors({});
    setAction({ status: "pending" });
    try {
      const saved = await api.saveModelPrice(editing);
      setPrices((current) => [...current.filter((price) => price.modelId !== saved.modelId), saved]
        .sort((left, right) => left.modelId.localeCompare(right.modelId)));
      restoreFocusRef.current = "trigger";
      setEditing(null);
      setEditingExisting(false);
      setAction({ status: "success", value: "定价已保存；重新扫描历史记录后会重算历史费用" });
    } catch (reason) {
      const correction = priceSaveCorrection(reason);
      if (correction.field) setFieldErrors({ [correction.field]: correction.message });
      else setError(correction.message);
      setAction({ status: "idle" });
    } finally {
      actionLock.current = false;
    }
  };
  const confirmDelete = async () => {
    if (!confirmation || actionLock.current || actionPending) return;
    const { modelId } = confirmation;
    actionLock.current = true;
    setAction({ status: "pending" });
    try {
      const deleted = await api.deleteModelPrice(modelId);
      setPrices((current) => current.filter((price) => price.modelId !== deleted));
      restoreFocusRef.current = "fallback";
      setConfirmation(null);
      setEditing(null);
      setEditingExisting(false);
      setAction({ status: "success", value: "定价已删除" });
    } catch {
      setDeleteError("删除定价失败，请重试");
      setAction({ status: "idle" });
    } finally {
      actionLock.current = false;
    }
  };
  const requestDelete = (modelId: string, trigger: HTMLButtonElement) => {
    if (actionLock.current || actionPending) return;
    returnFocusRef.current = trigger;
    setEditing(null);
    setConfirmation({ modelId });
    setAction({ status: "idle" });
    setDeleteError(null);
  };
  const cancelDelete = () => {
    restoreFocusRef.current = "trigger";
    setConfirmation(null);
    setDeleteError(null);
  };
  const actionMessage = action.status === "success" ? action.value : null;

  return <section className="settings-group price-overrides" aria-label="模型定价">
    <div className="section-heading">
      <div className="section-title">
        <h2 id="model-prices-title">模型定价</h2>
        <p>单位为 USD / 百万 Token。所有定价均可编辑或删除；保存后用于后续扫描，重新扫描历史记录可重算历史费用。</p>
      </div>
      {!editing && resource.state.status === "success" && <div className="section-actions">
        <button ref={addButtonRef} type="button" className="primary price-add" disabled={actionPending} onClick={(event) => beginEdit(undefined, event.currentTarget)}>＋ 添加</button>
      </div>}
    </div>
    {resource.state.status === "pending" && <p className="muted">正在读取定价…</p>}
    {resource.state.status === "error" && <p className="action-message failed" role="alert">{resource.state.error} <button type="button" className="secondary" onClick={resource.retry}>重试</button></p>}
    {editing && <fieldset ref={editorRef} className="price-editor">
      <legend>{editingExisting ? `编辑 ${editing.modelId} 的模型定价` : "添加模型定价"}</legend>
      <PriceField id="price-model-id" label={priceFieldLabels.modelId} value={editing.modelId} error={fieldErrors.modelId} disabled={editingExisting || actionPending} placeholder="claude-sonnet-4-6" maxLength={512} onChange={(modelId) => update({ modelId })} />
      <PriceField id="price-input" label={priceFieldLabels.inputCostPerMillion} value={editing.inputCostPerMillion} error={fieldErrors.inputCostPerMillion} disabled={actionPending} placeholder="3" onChange={(inputCostPerMillion) => update({ inputCostPerMillion })} />
      <PriceField id="price-output" label={priceFieldLabels.outputCostPerMillion} value={editing.outputCostPerMillion} error={fieldErrors.outputCostPerMillion} disabled={actionPending} placeholder="15" onChange={(outputCostPerMillion) => update({ outputCostPerMillion })} />
      <PriceField id="price-cache-write" label={priceFieldLabels.cacheWriteCostPerMillion} value={editing.cacheWriteCostPerMillion} error={fieldErrors.cacheWriteCostPerMillion} disabled={actionPending} onChange={(cacheWriteCostPerMillion) => update({ cacheWriteCostPerMillion })} />
      <PriceField id="price-cache-read" label={priceFieldLabels.cacheReadCostPerMillion} value={editing.cacheReadCostPerMillion} error={fieldErrors.cacheReadCostPerMillion} disabled={actionPending} onChange={(cacheReadCostPerMillion) => update({ cacheReadCostPerMillion })} />
      {error && <p className="action-message failed" role="alert">{error}</p>}
      <div className="price-actions">
        <button type="button" className="primary" disabled={actionPending} onClick={() => void save()}>{actionPending ? "正在保存…" : "保存定价"}</button>
        <button type="button" className="secondary" disabled={actionPending} onClick={closeEditor}>取消</button>
      </div>
    </fieldset>}
    {confirmation && <div className="price-confirmation" role="group" aria-label="确认删除定价">
      <p>{`确认删除 ${confirmation.modelId} 的定价？删除后该模型的未来用量将显示为未计费；重新扫描会按当前定价重算历史费用。`}</p>
      {deleteError && <p className="action-message failed" role="alert">{deleteError}</p>}
      <button ref={confirmationButtonRef} type="button" className="danger" disabled={actionPending} aria-busy={actionPending} onClick={() => void confirmDelete()}>{actionPending ? "正在删除…" : "确认删除"}</button>
      <button type="button" className="secondary" disabled={actionPending} onClick={cancelDelete}>取消</button>
    </div>}
    {resource.state.status === "success" && <>
      {prices.length === 0 ? <p className="muted">暂无定价。</p> : <div className="price-table-wrap"><table className="price-table">
        <caption>每百万 Token 的模型定价（USD）</caption>
        <thead><tr><th scope="col">模型</th><th scope="col">输入</th><th scope="col">输出</th><th scope="col">缓存读取</th><th scope="col">缓存创建</th><th scope="col">操作</th></tr></thead>
        <tbody>{prices.map((price) => <tr key={price.modelId}>
          <th scope="row"><strong>{price.modelId}</strong></th>
          <td>${price.inputCostPerMillion}</td>
          <td>${price.outputCostPerMillion}</td>
          <td>${price.cacheReadCostPerMillion}</td>
          <td>${price.cacheWriteCostPerMillion}</td>
          <td><div>
            <button type="button" className="secondary" disabled={actionPending} onClick={(event) => beginEdit(price, event.currentTarget)}>编辑</button>
            <button type="button" className="danger" disabled={actionPending} onClick={(event) => requestDelete(price.modelId, event.currentTarget)}>删除</button>
          </div></td>
        </tr>)}</tbody>
      </table></div>}
    </>}
    {actionMessage && <p className="action-message" role="status">{actionMessage}</p>}
  </section>;
}
