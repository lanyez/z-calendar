param([string]$path, [int]$x, [int]$y, [int]$w, [int]$h)
Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile($path)
$hist = @{}
for ($j = $y; $j -lt $y + $h; $j++) {
  for ($i = $x; $i -lt $x + $w; $i++) {
    $c = $bmp.GetPixel($i, $j)
    $key = "{0:D3},{1:D3},{2:D3}" -f $c.R, $c.G, $c.B
    if ($hist.ContainsKey($key)) { $hist[$key]++ } else { $hist[$key] = 1 }
  }
}
$bmp.Dispose()
Write-Output ("total distinct colors: " + $hist.Count)
$hist.GetEnumerator() | Sort-Object Value -Descending | Select-Object -First 40 | ForEach-Object {
  $rgb = $_.Key.Split(',')
  $r = [int]$rgb[0]; $g = [int]$rgb[1]; $b = [int]$rgb[2]
  $max = [Math]::Max($r, [Math]::Max($g, $b)); $min = [Math]::Min($r, [Math]::Min($g, $b))
  $tag = if ($max - $min -le 8) { "GRAY" } else { "TINTED R>B=$($r - $b)" }
  Write-Output ("{0}  x{1}  {2}" -f $_.Key, $_.Value, $tag)
}
