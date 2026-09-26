import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import "./app.css";

const root = document.getElementById("root");
if (!root) throw new Error("Missing #root element in the desktop entry document.");

createRoot(root).render(<App />);
