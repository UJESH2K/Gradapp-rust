// Theme, applied before first paint (a classic script in <head>, so the window
// never flashes dark before switching to light). Settings › Appearance › Theme
// is "system" | "dark" | "light"; the saved choice lives in the backend's
// settings.json, and a copy in localStorage lets this script apply it before
// the backend answers. "system" follows prefers-color-scheme live.
// Browser-only knob for screenshots: ?theme=light|dark|system.
(function () {
  var KEY = "gradapp:theme";
  var media = window.matchMedia ? window.matchMedia("(prefers-color-scheme: light)") : null;
  var pref = "system";
  try {
    pref = localStorage.getItem(KEY) || "system";
  } catch (e) {}
  var knob = /[?&]theme=(light|dark|system)\b/.exec(location.search);
  if (knob && !(window.__TAURI__ && window.__TAURI__.core)) pref = knob[1];

  function resolve(p) {
    if (p === "light" || p === "dark") return p;
    return media && media.matches ? "light" : "dark";
  }

  function apply(next) {
    pref = next === "light" || next === "dark" ? next : "system";
    try {
      localStorage.setItem(KEY, pref);
    } catch (e) {}
    var theme = resolve(pref);
    var root = document.documentElement;
    if (root.getAttribute("data-theme") !== theme) {
      root.setAttribute("data-theme", theme);
      window.dispatchEvent(new CustomEvent("gradapp:theme", { detail: theme }));
    }
    return theme;
  }

  if (media) {
    var onChange = function () {
      if (pref === "system") apply("system");
    };
    if (media.addEventListener) media.addEventListener("change", onChange);
    else if (media.addListener) media.addListener(onChange);
  }

  window.GradAppTheme = {
    apply: apply,
    current: function () {
      return document.documentElement.getAttribute("data-theme") || "dark";
    },
    preference: function () {
      return pref;
    },
  };
  document.documentElement.setAttribute("data-theme", resolve(pref));
})();
