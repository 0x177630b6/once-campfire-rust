(function () {
  var KEY = "hermes-theme", root = document.documentElement;
  // The browser chrome's colours (campfire_views::hermes::THEME_COLOR_LIGHT / _DARK).
  var CHROME = { light: "#f4f0e8", dark: "#181e1b" };
  function stored() { try { return localStorage.getItem(KEY); } catch (e) { return null; } }
  function apply(theme) {
    var chosen = theme === "light" || theme === "dark";
    if (chosen) root.setAttribute("data-theme", theme);
    else root.removeAttribute("data-theme");
    // The theme-color metas: each its own mode's colour for System, else both the chosen mode's.
    var metas = document.querySelectorAll('meta[name="theme-color"]');
    for (var i = 0; i < metas.length; i++) {
      var own = /dark/.test(metas[i].getAttribute("media") || "") ? "dark" : "light";
      metas[i].setAttribute("content", CHROME[chosen ? theme : own]);
    }
  }
  function sync() {
    apply(stored());
    var current = root.getAttribute("data-theme") || "system";
    var inputs = document.querySelectorAll('input[name="' + KEY + '"]');
    for (var i = 0; i < inputs.length; i++) inputs[i].checked = inputs[i].value === current;
  }
  apply(stored());
  if (window.hermesTheme) return;
  window.hermesTheme = true;
  document.addEventListener("change", function (event) {
    var input = event.target;
    if (!input || input.name !== KEY) return;
    apply(input.value);
    try {
      if (input.value === "light" || input.value === "dark") localStorage.setItem(KEY, input.value);
      else localStorage.removeItem(KEY);
    } catch (e) {}
  });
  document.addEventListener("DOMContentLoaded", sync);
  document.addEventListener("turbo:load", sync);
  document.addEventListener("turbo:render", sync);
  window.addEventListener("storage", function (event) { if (event.key === KEY) sync(); });
})();
