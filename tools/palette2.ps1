Add-Type -AssemblyName System.Drawing
$bmp=[System.Drawing.Bitmap]::FromFile('E:\git\calendar\1.png')
function Scan($name,$x0,$y0,$x1,$y1,$pred){
  $acc=@{}
  for($y=$y0;$y -lt $y1;$y++){ for($x=$x0;$x -lt $x1;$x++){
    $c=$bmp.GetPixel($x,$y)
    if((& $pred $c)){
      $key='#{0:X2}{1:X2}{2:X2}' -f $c.R,$c.G,$c.B
      if($acc.ContainsKey($key)){$acc[$key]++}else{$acc[$key]=1}
    }
  }}
  $top=$acc.GetEnumerator()|Sort-Object Value -Descending|Select-Object -First 4|ForEach-Object{ "$($_.Key) x$($_.Value)" }
  "$name : " + ($top -join ' | ')
}
Scan 'weekday-blue' 30 195 300 215 {param($c) ($c.B -gt $c.R+60) -and ($c.B -gt 180)}
Scan 'weekend-red' 300 195 430 215 {param($c) ($c.R -gt $c.B+60) -and ($c.R -gt 180)}
Scan 'lunar-gray' 30 215 260 232 {param($c) ([math]::Abs($c.R-$c.G) -lt 30) -and ([math]::Abs($c.G-$c.B) -lt 30) -and $c.R -gt 100 -and $c.R -lt 210}
Scan 'dim-othermonth' 30 195 100 232 {param($c) ($c.R -gt 40) -and ($c.R -lt 110)}
Scan 'term-blue' 30 340 430 400 {param($c) ($c.B -gt $c.R+80) -and ($c.B -gt 180) -and ($c.G -gt 120)}
Scan 'weeknum' 2 195 25 480 {param($c) $c.R -gt 90}
Scan 'badge-ban-red' 330 460 440 500 {param($c) ($c.R -gt $c.B+70) -and ($c.R -gt 180)}
Scan 'xiu-badge' 230 355 330 395 {param($c) ($c.B -gt $c.R+60)}
Scan 'header-weekday' 30 175 430 195 {param($c) ([math]::Abs($c.R-$c.G) -lt 30) -and ([math]::Abs($c.G-$c.B) -lt 30) -and $c.R -gt 90}
Scan 'date-line' 30 85 260 105 {param($c) ($c.R+$c.G+$c.B) -gt 420}
Scan 'lunar-in-date' 150 85 260 105 {param($c) ([math]::Abs($c.R-$c.G) -lt 30) -and $c.R -gt 80 -and $c.R -lt 200}
