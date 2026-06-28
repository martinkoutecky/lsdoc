// Standalone vitest project living in the scratchpad. Plain object (no import of
// vitest/config) so it loads without node_modules resolution from this dir. Root
// is this dir so the Tine repo tree is untouched; the runner imports
// parseInline.ts by absolute path.
export default {
  root: __dirname,
  test: {
    environment: "node",
    include: ["runner.test.ts"],
  },
  resolve: { conditions: ["browser"] },
};
