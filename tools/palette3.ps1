Add-Type -AssemblyName System.Drawing
$bmp=[System.Drawing.Bitmap]::FromFile('E:\git\calendar\1.png')
$acc=@{}
$ys=@{}
for($y=0;$y -lt $bmp.Height;$y++){ for($x=0;$x -lt $bmp.Width;$x++){
  $c=$bmp.GetPixel($x,$y)
  if(($c.R -gt $c.B+70) -and ($c.R -gt 170) -and ($c.G -lt 150)){
    $key='#{0:X2}{1:X2}{2:X2}' -f $c.R,$c.G,$c.B
    if($acc.ContainsKey($key)){$acc[$key]++}else{$acc[$key]=1}
    if($ys.ContainsKey($y)){$ys[$y]++}else{$ys[$y]=1}
  }
}}
"=== all red-ish colors ==="
$acc.GetEnumerator()|Sort-Object Value -Descending|Select-Object -First 10|ForEach-Object{ "$($_.Key) x$($_.Value)" }
"=== red y histogram (bucket 20) ==="
$ys.Keys|ForEach-Object{ [math]::Floor($_/20) }|Group-Object|Sort-Object {[int]$_.Name}|ForEach-Object{ "y $([int]$_.Name*20)-$([int]$_.Name*20+19): $($_.Count)" }
