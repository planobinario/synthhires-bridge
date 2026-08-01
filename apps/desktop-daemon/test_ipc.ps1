$pipeClient = New-Object System.IO.Pipes.NamedPipeClientStream(".", "synthhires-bridge-ipc", [System.IO.Pipes.PipeDirection]::InOut, [System.IO.Pipes.PipeOptions]::None)
Write-Host "Connecting to pipe..."
$pipeClient.Connect(5000)
Write-Host "Connected. Sending deep link..."
$writer = New-Object System.IO.StreamWriter($pipeClient)
$writer.Write("synthhires://pair?token=test_token_123")
$writer.Flush()

$reader = New-Object System.IO.StreamReader($pipeClient)
$response = $reader.ReadToEnd()
Write-Host "Response from Daemon: $response"
$pipeClient.Dispose()
