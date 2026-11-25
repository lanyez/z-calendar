Get-Process Z日历 -ErrorAction SilentlyContinue | ForEach-Object {
  Write-Output ("PID={0} WS={1}MB Private={2}MB" -f $_.Id, [math]::Round($_.WorkingSet64/1MB,1), [math]::Round($_.PrivateMemorySize64/1MB,1))
}
