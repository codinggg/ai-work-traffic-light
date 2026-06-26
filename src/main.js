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

  var currentStatus = "none";
  var currentFocused = false;
  var currentVertical = false;
  var currentLocked = false;

  var RESIZE_EDGE = 8;
  var MIN_SIZE_SCALE = 0.6;
  var MAX_SIZE_SCALE = 5;
  var BASE_SIZE = {
    horizontal: { width: 99, height: 33 },
    vertical: { width: 62, height: 166 },
  };
  var activeResize = null;

  var CURSOR_BY_DIRECTION = {
    North: "n-resize",
    South: "s-resize",
    East: "e-resize",
    West: "w-resize",
    NorthEast: "ne-resize",
    NorthWest: "nw-resize",
    SouthEast: "se-resize",
    SouthWest: "sw-resize",
  };

  function clamp(value, min, max) {
    return Math.max(min, Math.min(max, value));
  }

  function updateTrafficScale() {
    var base = currentVertical ? BASE_SIZE.vertical : BASE_SIZE.horizontal;
    var width = Math.max(1, window.innerWidth || base.width);
    var height = Math.max(1, window.innerHeight || base.height);
    var scale = clamp(Math.min(width / base.width, height / base.height), MIN_SIZE_SCALE, MAX_SIZE_SCALE);
    widget.style.setProperty("--traffic-scale", scale.toFixed(3));
  }

  function syncLayoutWindowSize(vertical) {
    var T = window.__TAURI__;
    if (T && T.core && typeof T.core.invoke === "function") {
      return T.core.invoke("set_light_layout_size", { vertical: !!vertical });
    }
    return Promise.resolve();
  }

  function applyLayout(vertical) {
    var next = !!vertical;
    currentVertical = next;
    syncLayoutWindowSize(next)
      .catch(function () {})
      .then(function () {
        widget.classList.toggle("is-vertical", next);
        widget.removeAttribute("title");
        updateTrafficScale();
      });
  }

  function getResizeDirection(e) {
    if (currentLocked || widget.classList.contains("is-none")) return null;

    var width = window.innerWidth || document.documentElement.clientWidth || 0;
    var height = window.innerHeight || document.documentElement.clientHeight || 0;
    if (width <= 0 || height <= 0) return null;

    var north = e.clientY <= RESIZE_EDGE;
    var south = e.clientY >= height - RESIZE_EDGE;
    var west = e.clientX <= RESIZE_EDGE;
    var east = e.clientX >= width - RESIZE_EDGE;

    if (north && east) return "NorthEast";
    if (north && west) return "NorthWest";
    if (south && east) return "SouthEast";
    if (south && west) return "SouthWest";
    if (north) return "North";
    if (south) return "South";
    if (east) return "East";
    if (west) return "West";
    return null;
  }

  function getCurrentWindow() {
    var T = window.__TAURI__;
    if (T && T.window && typeof T.window.getCurrentWindow === "function") {
      return T.window.getCurrentWindow();
    }
    return null;
  }

  function invokeWindowCommand(name, args) {
    var T = window.__TAURI__;
    if (T && T.core && typeof T.core.invoke === "function") {
      return T.core.invoke("plugin:window|" + name, args || {});
    }
    return Promise.resolve();
  }

  function startWindowDrag() {
    var win = getCurrentWindow();
    if (win && typeof win.startDragging === "function") {
      return win.startDragging();
    }
    return invokeWindowCommand("start_dragging");
  }

  function startWindowResize(direction, e) {
    var T = window.__TAURI__;
    if (!T || !T.core || typeof T.core.invoke !== "function") {
      return Promise.resolve();
    }
    return T.core.invoke("get_light_window_geometry").then(function (geometry) {
      activeResize = {
        direction: direction,
        startScreenX: e.screenX,
        startScreenY: e.screenY,
        geometry: geometry,
        pointerId: null,
      };
    });
  }

  function resizeScaleFromPointer(active, e) {
    var base = currentVertical ? BASE_SIZE.vertical : BASE_SIZE.horizontal;
    var dx = e.screenX - active.startScreenX;
    var dy = e.screenY - active.startScreenY;
    var candidates = [];

    if (active.direction.indexOf("East") !== -1) {
      candidates.push((active.geometry.width + dx) / base.width);
    }
    if (active.direction.indexOf("West") !== -1) {
      candidates.push((active.geometry.width - dx) / base.width);
    }
    if (active.direction.indexOf("South") !== -1) {
      candidates.push((active.geometry.height + dy) / base.height);
    }
    if (active.direction.indexOf("North") !== -1) {
      candidates.push((active.geometry.height - dy) / base.height);
    }

    if (!candidates.length) {
      candidates.push(active.geometry.width / base.width);
    }

    return clamp(Math.max.apply(Math, candidates), MIN_SIZE_SCALE, MAX_SIZE_SCALE);
  }

  function applyCustomResize(e) {
    if (!activeResize) return;

    var T = window.__TAURI__;
    if (!T || !T.core || typeof T.core.invoke !== "function") return;

    var base = currentVertical ? BASE_SIZE.vertical : BASE_SIZE.horizontal;
    var scale = resizeScaleFromPointer(activeResize, e);
    var width = base.width * scale;
    var height = base.height * scale;
    var x = activeResize.geometry.x;
    var y = activeResize.geometry.y;

    if (activeResize.direction.indexOf("West") !== -1) {
      x = activeResize.geometry.x + activeResize.geometry.width - width;
    }
    if (activeResize.direction.indexOf("North") !== -1) {
      y = activeResize.geometry.y + activeResize.geometry.height - height;
    }

    T.core
      .invoke("set_light_window_geometry", {
        vertical: currentVertical,
        width: width,
        height: height,
        x: x,
        y: y,
      })
      .catch(function () {});
  }

  function endCustomResize(e) {
    if (!activeResize) return;
    applyCustomResize(e);
    if (activeResize.pointerId !== null && widget.releasePointerCapture) {
      try {
        widget.releasePointerCapture(activeResize.pointerId);
      } catch (_) {}
    }
    activeResize = null;
    document.removeEventListener("pointermove", applyCustomResize, true);
    document.removeEventListener("pointerup", endCustomResize, true);
  }

  function updateResizeCursor(e) {
    var direction = getResizeDirection(e);
    document.body.style.cursor = direction ? CURSOR_BY_DIRECTION[direction] : "default";
  }

  document.addEventListener("mousemove", updateResizeCursor, true);
  document.addEventListener("mouseleave", function () {
    document.body.style.cursor = "default";
  });
  document.addEventListener(
    "pointerdown",
    function (e) {
      if (e.button !== 0 || currentLocked || widget.classList.contains("is-none")) return;

      var direction = getResizeDirection(e);
      e.preventDefault();
      e.stopPropagation();

      if (direction) {
        startWindowResize(direction, e)
          .then(function () {
            if (!activeResize) return;
            activeResize.startScreenX = e.screenX;
            activeResize.startScreenY = e.screenY;
            activeResize.pointerId = e.pointerId;
            if (widget.setPointerCapture) {
              try {
                widget.setPointerCapture(e.pointerId);
              } catch (_) {}
            }
            document.addEventListener("pointermove", applyCustomResize, true);
            document.addEventListener("pointerup", endCustomResize, true);
          })
          .catch(function () {});
      } else {
        startWindowDrag().catch(function () {});
      }
    },
    true
  );
  window.addEventListener("resize", updateTrafficScale);

  function updateAck() {
    if (currentFocused) {
      widget.classList.add("is-ack");
      if (currentStatus === "idle" || currentStatus === "error") {
        widget.classList.add("yellow-acked");
      }
    } else {
      if ((currentStatus === "idle" || currentStatus === "error") && widget.classList.contains("yellow-acked")) {
        widget.classList.add("is-ack");
      } else {
        widget.classList.remove("is-ack");
      }
    }
  }

  // 把一个聚合状态应用到灯。
  function applyState(state) {
    var status = (state && state.status) || "none";
    var cls = STATUS_CLASS[status] || "is-none";

    if (status !== currentStatus) {
      widget.classList.remove("yellow-acked");
      currentStatus = status;
    }

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
    } else if (status === "error") {
      labelEl.textContent = "";
    } else {
      labelEl.textContent = "";
    }
    widget.removeAttribute("title");

    updateAck();
  }

  // 暴露给后端/调试调用。
  window.TrafficLight = { applyState: applyState, applyLayout: applyLayout };

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
    updateTrafficScale();
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
    }, 5000); // 对应 styles.css 里 boot 动画总时长(约 1.05s)，留余量
  }
  function tryListen() {
    var T = window.__TAURI__;
    if (T && T.event && typeof T.event.listen === "function") {
      T.event.listen("state-changed", function (evt) {
        applyState(evt && evt.payload ? evt.payload : { status: "none" });
      });
      T.event.listen("layout-changed", function (evt) {
        applyLayout(!!(evt && evt.payload));
      });
      T.event.listen("locked-changed", function (evt) {
        currentLocked = !!(evt && evt.payload);
        if (currentLocked) document.body.style.cursor = "default";
      });
      if (T.core && typeof T.core.invoke === "function") {
        T.core.invoke("get_light_layout").then(applyLayout).catch(function () {});
        T.core.invoke("get_locked")
          .then(function (locked) {
            currentLocked = !!locked;
          })
          .catch(function () {});
      }
      // 后端检测到前台是工作窗口(VSCode/终端/Claude)时 payload=true -> 灯常亮(停闪)；
      // 否则 false -> 红/黄灯恢复闪烁提醒。绿灯本来就不闪。
      // 需求：黄灯闪烁之后，如果切换到活动窗口后，就停止闪烁，切换其他窗口也不闪烁。
      T.event.listen("focus-changed", function (evt) {
        currentFocused = !!(evt && evt.payload);
        updateAck();
      });
      return true;
    }
    return false;
  }

  // 禁用右键：红绿灯不弹浏览器(WebView)右键菜单，右键无任何操作。
  // 用捕获阶段在 document 上拦下，确保最先吃到事件并取消默认菜单。
  // （引擎级还会在 Rust 里关掉 WebView2 默认菜单做双保险；锁定改走托盘菜单。）
  document.addEventListener(
    "contextmenu",
    function (e) {
      e.preventDefault();
      return false;
    },
    true
  );

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
