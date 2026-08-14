import { useCallback, useEffect, useRef, useState } from "react";

export type ActionState<T> =
  | { status: "idle" | "pending" }
  | { status: "success"; value: T }
  | { status: "error"; error: string };

export type ResourceState<T> =
  | { status: "pending" }
  | { status: "success"; value: T }
  | { status: "error"; error: string };

type ErrorMessage = string | ((reason: unknown) => string);

function messageFor(errorMessage: ErrorMessage, reason: unknown) {
  return typeof errorMessage === "function" ? errorMessage(reason) : errorMessage;
}

export function useAsyncAction<T>(action: () => Promise<T>, errorMessage: ErrorMessage) {
  const [state, setState] = useState<ActionState<T>>({ status: "idle" });
  const requestId = useRef(0);
  const run = useCallback(async () => {
    const current = ++requestId.current;
    setState({ status: "pending" });
    try {
      const value = await action();
      if (requestId.current === current) setState({ status: "success", value });
      return value;
    } catch (reason) {
      if (requestId.current === current) {
        setState({ status: "error", error: messageFor(errorMessage, reason) });
      }
      throw reason;
    }
  }, [action, errorMessage]);
  const reset = useCallback(() => {
    requestId.current++;
    setState({ status: "idle" });
  }, []);
  return { state, run, reset };
}

export function useAsyncResource<T>(load: () => Promise<T>, errorMessage: string) {
  const [state, setState] = useState<ResourceState<T>>({ status: "pending" });
  const [attempt, setAttempt] = useState(0);
  const requestId = useRef(0);
  useEffect(() => {
    const current = ++requestId.current;
    setState({ status: "pending" });
    void load().then(
      (value) => {
        if (requestId.current === current) setState({ status: "success", value });
      },
      () => {
        if (requestId.current === current) setState({ status: "error", error: errorMessage });
      },
    );
    return () => {
      if (requestId.current === current) requestId.current++;
    };
  }, [attempt, errorMessage, load]);
  const retry = useCallback(() => setAttempt((value) => value + 1), []);
  return { state, retry };
}
