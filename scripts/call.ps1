param(
    [Parameter(Mandatory = $true)]
    [string]$Method,
    [hashtable]$Params = @{},
    [int]$Port = 8765
)

$payload = @{
    id     = 1
    method = $Method
    params = $Params
} | ConvertTo-Json -Compress -Depth 8

$client = New-Object System.Net.Sockets.TcpClient
$client.Connect("127.0.0.1", $Port)
$stream = $client.GetStream()
$writer = New-Object System.IO.StreamWriter($stream)
$reader = New-Object System.IO.StreamReader($stream)
$writer.Write($payload)
$writer.Flush()
# half-close write side so server finishes reading
$client.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send)
$response = $reader.ReadToEnd()
$client.Close()
$response | ConvertFrom-Json | ConvertTo-Json -Depth 10
