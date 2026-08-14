import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import App from "../../App";
import { api } from "../../api";

describe("Model pricing", () => {
  it("creates a model price outside the remote-notification form", async () => {
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "＋ 添加" }));
    expect(screen.getByRole("group", { name: "添加模型定价" })).toBeTruthy();
    const modelId = screen.getByLabelText("模型 ID") as HTMLInputElement;
    await waitFor(() => expect(document.activeElement).toBe(modelId));
    expect(modelId.closest("form")).toBeNull();
    fireEvent.keyDown(modelId, { key: "Enter", code: "Enter" });
    expect(api.saveSettings).not.toHaveBeenCalled();
    fireEvent.change(modelId, { target: { value: "test-model" } });
    fireEvent.change(screen.getByLabelText("输入 Token 单价"), { target: { value: "1.25" } });
    fireEvent.change(screen.getByLabelText("输出 Token 单价"), { target: { value: "5" } });
    fireEvent.change(screen.getByLabelText("缓存创建单价"), { target: { value: "1.5" } });
    fireEvent.change(screen.getByLabelText("缓存读取单价"), { target: { value: "0.25" } });
    fireEvent.click(screen.getByRole("button", { name: "保存定价" }));
    await waitFor(() => expect(api.saveModelPrice).toHaveBeenCalledWith({
      modelId: "test-model", inputCostPerMillion: "1.25", outputCostPerMillion: "5",
      cacheWriteCostPerMillion: "1.5", cacheReadCostPerMillion: "0.25",
    }));
    expect(await screen.findByText("定价已保存；重新扫描历史记录后会重算历史费用")).toBeTruthy();
  });

  it("validates model-id bounds and pico-USD range inside the editor", async () => {
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "＋ 添加" }));
    fireEvent.change(screen.getByLabelText("模型 ID"), { target: { value: "x".repeat(257) } });
    fireEvent.change(screen.getByLabelText("输入 Token 单价"), { target: { value: "1" } });
    fireEvent.change(screen.getByLabelText("输出 Token 单价"), { target: { value: "2" } });
    fireEvent.click(screen.getByRole("button", { name: "保存定价" }));
    expect(await screen.findByText("模型 ID 不能超过 256 字节")).toBeTruthy();
    expect((screen.getByLabelText("模型 ID") as HTMLInputElement).getAttribute("aria-invalid")).toBe("true");
    expect(api.saveModelPrice).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText("模型 ID"), { target: { value: "bounded-model" } });
    fireEvent.change(screen.getByLabelText("输入 Token 单价"), { target: { value: "9223372036854775808" } });
    fireEvent.click(screen.getByRole("button", { name: "保存定价" }));
    expect(await screen.findByText("金额超出可保存范围")).toBeTruthy();
    expect(document.activeElement).toBe(screen.getByLabelText("输入 Token 单价"));
    expect(api.saveModelPrice).not.toHaveBeenCalled();
  });

  it("maps backend price rejection to the correct editor field", async () => {
    vi.mocked(api.saveModelPrice).mockRejectedValueOnce("price_output_out_of_range");
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "＋ 添加" }));
    fireEvent.change(screen.getByLabelText("模型 ID"), { target: { value: "backend-model" } });
    fireEvent.change(screen.getByLabelText("输入 Token 单价"), { target: { value: "1" } });
    fireEvent.change(screen.getByLabelText("输出 Token 单价"), { target: { value: "2" } });
    fireEvent.click(screen.getByRole("button", { name: "保存定价" }));

    expect(await screen.findByText("输出 Token 单价超出可保存范围")).toBeTruthy();
    const output = screen.getByLabelText("输出 Token 单价") as HTMLInputElement;
    expect(output.getAttribute("aria-invalid")).toBe("true");
    await waitFor(() => expect(document.activeElement).toBe(output));
    expect(screen.getByRole("group", { name: "添加模型定价" })).toBeTruthy();
  });

  it("presents every listed price as the same editable and deletable entry", async () => {
    vi.mocked(api.modelPrices).mockResolvedValueOnce([{
      modelId: "claude-sonnet-4-6", inputCostPerMillion: "3", outputCostPerMillion: "15",
      cacheWriteCostPerMillion: "3.75", cacheReadCostPerMillion: "0.3",
    }, {
      modelId: "custom-model", inputCostPerMillion: "1", outputCostPerMillion: "2",
      cacheWriteCostPerMillion: "3", cacheReadCostPerMillion: "4",
    }]);
    location.hash = "settings";
    render(<App />);
    const pricingTable = await screen.findByRole("table", { name: "每百万 Token 的模型定价（USD）" });
    expect(within(pricingTable).getAllByRole("columnheader")).toHaveLength(6);
    for (const model of ["claude-sonnet-4-6", "custom-model"]) {
      const row = within(pricingTable).getByRole("rowheader", { name: model }).closest("tr")!;
      expect(within(row).getByRole("button", { name: "编辑" })).toBeTruthy();
      expect(within(row).getByRole("button", { name: "删除" })).toBeTruthy();
    }
    const pricing = screen.getByRole("region", { name: "模型定价" });
    for (const hiddenConcept of ["内置", "自定义", "已停用", "停用定价", "恢复内置定价", "恢复默认值"]) {
      expect(within(pricing).queryByText(hiddenConcept, { exact: false })).toBeNull();
    }

    const editTrigger = within(within(pricingTable).getByRole("rowheader", { name: "claude-sonnet-4-6" }).closest("tr")!).getByRole("button", { name: "编辑" });
    fireEvent.click(editTrigger);
    expect(screen.getByRole("group", { name: "编辑 claude-sonnet-4-6 的模型定价" })).toBeTruthy();
    const modelId = screen.getByLabelText("模型 ID") as HTMLInputElement;
    expect(modelId.value).toBe("claude-sonnet-4-6");
    expect(modelId.disabled).toBe(true);
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("输入 Token 单价")));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    await waitFor(() => expect(document.activeElement).toBe(editTrigger));

    fireEvent.click(editTrigger);
    fireEvent.change(screen.getByLabelText("输入 Token 单价"), { target: { value: "4" } });
    fireEvent.click(screen.getByRole("button", { name: "保存定价" }));
    await waitFor(() => expect(api.saveModelPrice).toHaveBeenCalledWith(expect.objectContaining({
      modelId: "claude-sonnet-4-6", inputCostPerMillion: "4",
    })));
  });

  it("uses one delete confirmation and restores focus safely", async () => {
    vi.mocked(api.modelPrices)
      .mockResolvedValueOnce([{
      modelId: "claude-sonnet-4-6", inputCostPerMillion: "3", outputCostPerMillion: "15",
      cacheWriteCostPerMillion: "3.75", cacheReadCostPerMillion: "0.3",
      }])
      .mockResolvedValueOnce([]);
    location.hash = "settings";
    render(<App />);
    const deleteTrigger = await screen.findByRole("button", { name: "删除" });
    fireEvent.click(deleteTrigger);
    expect(api.deleteModelPrice).not.toHaveBeenCalled();
    const confirmation = screen.getByRole("group", { name: "确认删除定价" });
    expect(within(confirmation).getByText(/未来用量将显示为未计费；重新扫描会按当前定价重算历史费用/)).toBeTruthy();
    const confirmButton = within(confirmation).getByRole("button", { name: "确认删除" });
    await waitFor(() => expect(document.activeElement).toBe(confirmButton));
    fireEvent.click(within(confirmation).getByRole("button", { name: "取消" }));
    await waitFor(() => expect(document.activeElement).toBe(deleteTrigger));

    fireEvent.click(deleteTrigger);
    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(api.deleteModelPrice).toHaveBeenCalledWith("claude-sonnet-4-6"));
    expect(await screen.findByText("定价已删除")).toBeTruthy();
    const add = await screen.findByRole("button", { name: "＋ 添加" });
    await waitFor(() => expect(document.activeElement).toBe(add));
  });

  it("keeps a delete failure and useful focus inside its confirmation", async () => {
    vi.mocked(api.modelPrices).mockResolvedValueOnce([{
      modelId: "delete-failure-model", inputCostPerMillion: "1", outputCostPerMillion: "2",
      cacheWriteCostPerMillion: "3", cacheReadCostPerMillion: "4",
    }]);
    vi.mocked(api.deleteModelPrice).mockRejectedValueOnce("price_storage_failed");
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "删除" }));
    const confirm = screen.getByRole("button", { name: "确认删除" });
    fireEvent.click(confirm);

    const group = await screen.findByRole("group", { name: "确认删除定价" });
    expect(within(group).getByRole("alert").textContent).toBe("删除定价失败，请重试");
    expect(document.activeElement).toBe(confirm);
    expect(within(group).getByRole("button", { name: "取消" })).toBeTruthy();
  });

  it("locks every pricing mutation while deletion is pending", async () => {
    let finishDelete!: () => void;
    const pendingDelete = new Promise<string>((resolve) => {
      finishDelete = () => resolve("default-model");
    });
    vi.mocked(api.deleteModelPrice).mockReturnValueOnce(pendingDelete);
    vi.mocked(api.modelPrices).mockResolvedValue([
      { modelId: "default-model", inputCostPerMillion: "5", outputCostPerMillion: "6", cacheWriteCostPerMillion: "7", cacheReadCostPerMillion: "8" },
      { modelId: "custom-model", inputCostPerMillion: "1", outputCostPerMillion: "2", cacheWriteCostPerMillion: "3", cacheReadCostPerMillion: "4" },
    ]);
    location.hash = "settings";
    render(<App />);
    fireEvent.click((await screen.findAllByRole("button", { name: "删除" }))[0]);
    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(api.deleteModelPrice).toHaveBeenCalledOnce());

    const pendingConfirmation = screen.getByRole("button", { name: "正在删除…" }) as HTMLButtonElement;
    expect(pendingConfirmation.disabled).toBe(true);
    expect((screen.getByRole("button", { name: "取消" }) as HTMLButtonElement).disabled).toBe(true);
    for (const name of ["＋ 添加", "编辑", "删除"]) {
      for (const button of screen.getAllByRole("button", { name })) {
        expect((button as HTMLButtonElement).disabled).toBe(true);
        fireEvent.click(button);
      }
    }
    fireEvent.click(pendingConfirmation);
    expect(api.deleteModelPrice).toHaveBeenCalledOnce();
    expect(api.saveModelPrice).not.toHaveBeenCalled();

    await act(async () => finishDelete());
    expect(await screen.findByText("定价已删除")).toBeTruthy();
  });
});
