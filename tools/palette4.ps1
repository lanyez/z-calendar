Add-Type -AssemblyName System.Drawing
$bmp=[System.Drawing.Bitmap]::FromFile('E:\git\calendar\1.png')
function Cell($name,$x0,$y0,$x1,$y1){
  $acc=@{}
  for($y=$y0;$y -lt $y1;$y++){ for($x=$x0;$x -lt $x1;$x++){
    $c=$bmp.GetPixel($x,$y)
    if($c.R+$c.G+$c.B -gt 180){
      $key='#{0:X2}{1:X2}{2:X2}' -f $c.R,$c.G,$c.B
      if($acc.ContainsKey($key)){$acc[$key]++}else{$acc[$key]=1}
    }
  }}
  $top=$acc.GetEnumerator()|Sort-Object Value -Descending|Select-Object -First 3|ForEach-Object{ "$($_.Key) x$($_.Value)" }
  "$name : " + ($top -join ' | ')
}
Cell 'num-31-prevmonth' 40 205 75 245
Cell 'num-1-oct'        215 410 255 450
Cell 'num-11-oct'       395 465 432 505
Cell 'num-28-sep'       40 410 75 450
Cell 'sub-31'           40 232 75 252
Cell 'sub-1-gq'         205 432 260 455
Cell 'num-14-sep'       40 305 75 345
