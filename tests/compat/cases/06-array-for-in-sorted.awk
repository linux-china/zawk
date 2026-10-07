# stdin: data/countries
function isort(arr, n,   i, j, t) {
  for (i = 2; i <= n; i++)
    for (j = i; j > 1 && arr[j - 1] > arr[j]; j--) { t = arr[j]; arr[j] = arr[j - 1]; arr[j - 1] = t }
}
{ pop[$4] += $3 }
END { n = 0; for (c in pop) keys[++n] = c; isort(keys, n); for (i = 1; i <= n; i++) print keys[i], pop[keys[i]] }
