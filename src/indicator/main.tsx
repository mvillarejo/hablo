import React from "react";
import ReactDOM from "react-dom/client";
import "../gladia-tokens.css";
import "./indicator.css";
import { IndicatorApp } from "./IndicatorApp";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <IndicatorApp />
  </React.StrictMode>,
);
