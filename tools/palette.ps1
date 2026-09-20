Add-Type -AssemblyName System.Drawing
$bmp=[System.Drawing.Bitmap]::FromFile('E:\git\calendar\1.png')
"size: $($bmp.Width)x$($bmp.Height)"
function Scan($name,$x0,$y0,$x1,$y1,$pred){
  $acc=@{}
  for($y=$y0;$y -lt $y1;$y++){ for($x=$x0;$x -lt $x1;$x++){
    $c=$bmp.GetPixel($x,$y)
    if((& $pred $c)){
      $key='#{0:X2}{1:X2}{2:X2}' -f ([int]($c.R/8)*8),([int]($c.G/8)*8),([int]($c.B/8)*8)
      if($acc.ContainsKey($key)){$acc[$key]++}else{$acc[$key]=1}
    }
  }}
  $top=$acc.GetEnumerator()|Sort-Object Value -Descending|Select-Object -First 5|ForEach-Object{ "$($_.Key) x$($_.Value)" }
  "$name : " + ($top -join ' | ')
}
Scan 'bg-main' 180 120 300 150 {param($c) $true}
Scan 'big-time' 30 30 250 65 {param($c) ($c.R+$c.G+$c.B) -gt 600}
Scan 'blue-numbers' 30 182 300 200 {param($c) ($c.B -gt $c.R+50) -and ($c.B -gt 150)}
Scan 'red-numbers' 300 182 430 200 {param($c) ($c.R -gt $c.B+50) -and ($c.R -gt 150)}
Scan 'today-circle' 235 280 290 320 {param($c) ($c.B -gt $c.R+60)}
Scan 'gray-lunar' 30 198 260 214 {param($c) ([math]::Abs($c.R-$c.G) -lt 25) -and ([math]::Abs($c.G-$c.B) -lt 25) -and $c.R -gt 90 -and $c.R -lt 200}
Scan 'bottom-bar' 180 570 260 600 {param($c) $true}
Scan 'badge-xiu' 230 330 300 365 {param($c) ($c.B -gt $c.R+40) -and ($c.G -gt $c.R)}
