(function () {
  var KEY = "hermes-theme", root = document.documentElement;
  function stored() { try { return localStorage.getItem(KEY); } catch (e) { return null; } }
  function apply(theme) {
    if (theme === "light" || theme === "dark") root.setAttribute("data-theme", theme);
    else root.removeAttribute("data-theme");
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
