import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@mantine/core/styles.css";
import "./styles.css";
import App from "./App";

const root = document.getElementById("root");
if (!root) throw new Error("Benchmark application root is missing.");

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
