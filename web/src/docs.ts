// The docs pages (scripts/docs.mjs): the contents fold away on narrow screens, and mermaid
// diagrams render in the brand's colors, loading mermaid only on pages that have one.

const contents = document.querySelector<HTMLDetailsElement>(".docs-nav");
if (contents && matchMedia("(max-width: 860px)").matches) contents.open = false;

const diagrams = Array.from(document.querySelectorAll<HTMLElement>("pre.mermaid"));
if (diagrams.length) {
  void import("mermaid").then(async ({ default: mermaid }) => {
    mermaid.initialize({
      startOnLoad: false,
      theme: "base",
      themeVariables: {
        background: "#1a1139",
        primaryColor: "#140c2e",
        primaryTextColor: "#edecfa",
        primaryBorderColor: "#3a3068",
        secondaryColor: "#140c2e",
        tertiaryColor: "#140c2e",
        lineColor: "#a39cc8",
        textColor: "#edecfa",
        edgeLabelBackground: "#1a1139",
        fontFamily: "IBM Plex Sans, system-ui, sans-serif",
        fontSize: "14px",
      },
    });
    await mermaid.run({ nodes: diagrams });
  });
}
