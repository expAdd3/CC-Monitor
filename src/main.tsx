import React from "react";
import ReactDOM from "react-dom/client";
import "./styles.css";

function App() {
  return (
    <main>
      <p className="eyebrow">CC MONITOR</p>
      <h1>Dashboard shell is ready.</h1>
      <p>Session monitoring surfaces are implemented in Phase 6.</p>
    </main>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
