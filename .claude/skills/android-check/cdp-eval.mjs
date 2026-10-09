// AndroidのWebViewで式を評価し、結果をJSONで出す(Chrome DevTools Protocol)。
// 使い方: node cdp-eval.mjs <port> '<式>'
// <port>は`adb forward tcp:<port> localabstract:webview_devtools_remote_<pid>`で転送したもの(SKILL.md 4節)。
const [port, expression] = process.argv.slice(2);
if (!port || !expression) {
  console.error("usage: node cdp-eval.mjs <port> '<expression>'");
  process.exit(2);
}
const pages = await (await fetch(`http://localhost:${port}/json`)).json();
const page = pages.find((p) => p.type === "page");
if (!page) {
  console.error("no page: the app is not running, or the forwarded socket belongs to an old process");
  process.exit(1);
}
const timer = setTimeout(() => {
  console.error("timed out: the page has not loaded (check the URL in /json)");
  process.exit(1);
}, 10000);
const ws = new WebSocket(page.webSocketDebuggerUrl);
ws.onopen = () =>
  ws.send(
    JSON.stringify({
      id: 1,
      method: "Runtime.evaluate",
      params: { expression, returnByValue: true, awaitPromise: true },
    }),
  );
ws.onmessage = (event) => {
  const message = JSON.parse(event.data);
  if (message.id !== 1) return;
  clearTimeout(timer);
  const result = message.result?.result;
  if (message.result?.exceptionDetails) {
    console.error(JSON.stringify(message.result.exceptionDetails, null, 1));
    process.exitCode = 1;
  } else {
    console.log(JSON.stringify(result?.value, null, 1));
  }
  ws.close();
};
