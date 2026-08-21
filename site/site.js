(function () {
  const navToggle = document.querySelector("#nav-toggle");
  if (navToggle) {
    navToggle.addEventListener("click", () => {
      const open = !document.body.classList.contains("nav-open");
      document.body.classList.toggle("nav-open", open);
      navToggle.setAttribute("aria-expanded", String(open));
    });
  }

  document.querySelectorAll(".copy-btn").forEach((button) => {
    button.addEventListener("click", async () => {
      const code = button.parentElement && button.parentElement.querySelector("code");
      const text = code ? code.innerText.replace(/^\$ /gm, "").trim() : "";
      if (!text) return;

      try {
        await navigator.clipboard.writeText(text);
        button.textContent = "copied";
      } catch {
        const input = document.createElement("textarea");
        input.value = text;
        input.setAttribute("readonly", "");
        input.style.position = "fixed";
        input.style.opacity = "0";
        document.body.appendChild(input);
        input.select();
        document.execCommand("copy");
        input.remove();
        button.textContent = "copied";
      }

      window.setTimeout(() => {
        button.textContent = "copy";
      }, 1400);
    });
  });
})();
