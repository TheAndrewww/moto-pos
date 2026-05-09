import React from "react";
import ReactDOM from "react-dom/client";
// Fuentes bundleadas localmente vía @fontsource — antes se cargaban desde
// fonts.googleapis.com con `@import url(...)`. Ese enfoque hacía que dev y
// builds frescas mostraran el fallback (system-ui) hasta que la red
// respondiera, mientras que la versión instalada ya tenía Inter en cache —
// resultando en fuentes distintas entre entornos. Con fontsource las
// fuentes viajan en el bundle y todos los entornos las usan instantáneo.
import "@fontsource/inter/400.css";
import "@fontsource/inter/500.css";
import "@fontsource/inter/600.css";
import "@fontsource/inter/700.css";
import "@fontsource/inter/800.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/600.css";
import "./index.css";
import App from "./App";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
