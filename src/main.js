// AI Work Traffic Light — 前端灯逻辑（无构建步骤，纯浏览器可跑）
//
// 聚合状态由后端(Tauri)通过 `state-changed` 事件推送：
//   { status: "working" | "idle" | "blocked" | "error" | "none", sessionLabel?: string }
//     working = 绿（Claude 工作中）
//     idle    = 黄（完成这轮 / 空闲，该你了）
//     blocked = 红（卡住，等你 —— 带会话标识）
//     error   = 黄（API 报错，如 429 限流/服务不可用 —— 来自扫 transcript）
//     none    = 隐藏（无任何会话）
//
// 浏览器下(无 Tauri)自动进入演示模式，可手动切换各状态验证视觉。

(function () {
  "use strict";

  var STATUS_CLASS = {
    working: "is-working",
    idle: "is-idle",
    blocked: "is-blocked",
    error: "is-error",
    neutral: "is-neutral",
    none: "is-none",
  };

  var widget = document.getElementById("widget");
  var labelEl = document.getElementById("label");

  // 把一个聚合状态应用到灯。
  function applyState(state) {
    var status = (state && state.status) || "none";
    var cls = STATUS_CLASS[status] || "is-none";

    widget.classList.remove(
      "is-working",
      "is-idle",
      "is-blocked",
      "is-error",
      "is-neutral",
      "is-none",
      "has-label"
    );
    widget.classList.add(cls);

    // 标签仅在红灯且有会话标识时显示——标出哪个会话需要你。
    var label = state && state.sessionLabel ? String(state.sessionLabel) : "";
    if (status === "blocked" && label) {
      labelEl.textContent = label;
      widget.classList.add("has-label");
      widget.setAttribute("title", "需要你：" + label);
    } else if (status === "error") {
      labelEl.textContent = "";
      widget.setAttribute("title", "API 报错（限流/服务不可用）");
    } else {
      labelEl.textContent = "";
      widget.setAttribute("title", "AI Work Traffic Light");
    }
  }

  // 暴露给后端/调试调用。
  window.TrafficLight = { applyState: applyState };

  // 演示模式仅在显式 ?demo 时开启；真实 app 永不进入演示
  // (否则深色底 + 演示面板会露出来，看起来像"黑框")。
  var demoMode = new URLSearchParams(window.location.search).has("demo");

  if (demoMode) {
    enableDemo();
  } else {
    listenForState();
  }

  // 监听后端 state-changed；__TAURI__ 可能稍后才注入，重试约 5 秒。
  function listenForState() {
    playBoot();
    if (tryListen()) return;
    var tries = 0;
    var timer = setInterval(function () {
      if (tryListen() || ++tries > 50) clearInterval(timer);
    }, 100);
  }

  // 启动开场：让灯立刻以 neutral 灰态显示出来(后端也会显示窗口)，
  // 并播放"红→黄→绿依次亮一下"动画，结束后回到 neutral 静态。
  function playBoot() {
    widget.classList.remove("is-none");
    widget.classList.add("is-neutral", "is-boot");
    setTimeout(function () {
      widget.classList.remove("is-boot");
    }, 3000); // 对应 styles.css 里 boot 动画总时长(约 1.05s)，留余量
  }
  function tryListen() {
    var T = window.__TAURI__;
    if (T && T.event && typeof T.event.listen === "function") {
      T.event.listen("state-changed", function (evt) {
        applyState(evt && evt.payload ? evt.payload : { status: "none" });
      });
      // 后端检测到前台是工作窗口(VSCode/终端/Claude)时 payload=true -> 灯常亮(停闪)；
      // 否则 false -> 红/黄灯恢复闪烁提醒。绿灯本来就不闪。
      T.event.listen("focus-changed", function (evt) {
        var atWork = !!(evt && evt.payload);
        widget.classList.toggle("is-ack", atWork);
      });
      return true;
    }
    return false;
  }

  // 禁用右键：红绿灯不弹浏览器(WebView)右键菜单，右键无任何操作。
  // （锁定位置等改从托盘右键菜单走，灯本身右键不再有菜单。）
  window.addEventListener("contextmenu", function (e) {
    e.preventDefault();
  });

  function enableDemo() {
    document.body.classList.add("demo-mode");
    var demo = document.getElementById("demo");
    if (demo) demo.hidden = false;

    var input = document.getElementById("demo-label");
    var buttons = document.querySelectorAll(".demo button[data-status]");
    Array.prototype.forEach.call(buttons, function (btn) {
      btn.addEventListener("click", function () {
        applyState({
          status: btn.getAttribute("data-status"),
          sessionLabel: input ? input.value : "",
        });
      });
    });

    applyState({ status: "working", sessionLabel: input ? input.value : "" });
  }
})();
