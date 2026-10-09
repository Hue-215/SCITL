// AndroidのWebViewで式を評価し、結果をJSONで出す(Chrome DevTools Protocol)。Node 22以上
// (組み込みの`WebSocket`を使う)。
// 使い方: node cdp-eval.mjs <port> '<式>'
// <port>は`adb forward tcp:<port> localabstract:webview_devtools_remote_<pid>`で転送したもの(SKILL.md 4節)。
const [port, expression] = process.argv.slice(2);
if (!port || !expression) {
  console.error("usage: node cdp-eval.mjs <port> '<expression>'");
  process.exit(2);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

let pages;
try {
  pages = await (await fetch(`http://localhost:${port}/json`)).json();
} catch (e) {
  fail(`cannot reach the forwarded port: ${e.cause?.code ?? e.message} (is the socket of the running process forwarded?)`);
}
const page = pages.find((p) => p.type === "page");
if (!page) fail("no page: the app is not running");

const timer = setTimeout(() => fail("timed out: the page has not loaded (check the URL in /json)"), 10000);
const ws = new WebSocket(page.webSocketDebuggerUrl);
ws.onerror = () => fail("cannot connect to the page's debugger");
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
  if (message.error) {
    console.error(JSON.stringify(message.error, null, 1));
    process.exitCode = 1;
  } else if (message.result.exceptionDetails) {
    console.error(JSON.stringify(message.result.exceptionDetails, null, 1));
    process.exitCode = 1;
  } else {
    console.log(JSON.stringify(message.result.result.value, null, 1));
  }
  ws.close();
};
