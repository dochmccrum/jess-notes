export function applyTheme(t: string) {
  if (t === 'system') document.documentElement.removeAttribute('data-theme')
  else document.documentElement.dataset.theme = t
}
