--!strict
-- NeuMan Studio Plugin 0.1.0
--
-- Local-only companion for the NeuMan desktop/daemon. This file is kept as a
-- single distributable Plugin script so enterprise installations can pin and
-- audit exactly what Studio executes. It never executes received Luau and it
-- never stores cloud credentials.

local HttpService = game:GetService("HttpService")
local ChangeHistoryService = game:GetService("ChangeHistoryService")
local Selection = game:GetService("Selection")
local RunService = game:GetService("RunService")
local StudioService = game:GetService("StudioService")
local Workspace = game:GetService("Workspace")
local ChangeHistoryAny = ChangeHistoryService :: any

local PLUGIN_VERSION = "0.1.0"
local PROTOCOL_VERSION = "1.0"
local DISCOVERY_URL = "http://127.0.0.1:34873/.well-known/neuman-studio-bridge"
local CELL_ATTRIBUTE = "NeuManCellId"
local PROJECT_ATTRIBUTE = "NeuManProjectId"
local INSTANCE_ATTRIBUTE = "NeuManInstanceId"
local MAX_CELL_BYTES = 96 * 1024 * 1024
local CHUNK_BYTES = 256 * 1024

type JsonMap = { [string]: any }
type ConnectionList = { RBXScriptConnection }

-- BLAKE3 --------------------------------------------------------------------
-- Protocol hashes are calculated in-plugin. This one-shot implementation is
-- deliberately self-tested at startup against the official empty-input vector.

local B3_IV = {
	0x6A09E667,
	0xBB67AE85,
	0x3C6EF372,
	0xA54FF53A,
	0x510E527F,
	0x9B05688C,
	0x1F83D9AB,
	0x5BE0CD19,
}
local B3_CHUNK_START = 1
local B3_CHUNK_END = 2
local B3_PARENT = 4
local B3_ROOT = 8
local B3_PERMUTATION = { 3, 7, 4, 11, 8, 1, 5, 14, 2, 12, 13, 6, 10, 15, 16, 9 }

local function add32(...: number): number
	local result = 0
	for index = 1, select("#", ...) do
		result = (result + (select(index, ...) :: number)) % 0x100000000
	end
	return result
end

local function copyWords(words: { number }): { number }
	local result = table.create(#words)
	for index, word in words do
		result[index] = word
	end
	return result
end

local function b3G(state: { number }, a: number, b: number, c: number, d: number, x: number, y: number)
	state[a] = add32(state[a], state[b], x)
	state[d] = bit32.rrotate(bit32.bxor(state[d], state[a]), 16)
	state[c] = add32(state[c], state[d])
	state[b] = bit32.rrotate(bit32.bxor(state[b], state[c]), 12)
	state[a] = add32(state[a], state[b], y)
	state[d] = bit32.rrotate(bit32.bxor(state[d], state[a]), 8)
	state[c] = add32(state[c], state[d])
	state[b] = bit32.rrotate(bit32.bxor(state[b], state[c]), 7)
end

local function b3Round(state: { number }, message: { number })
	b3G(state, 1, 5, 9, 13, message[1], message[2])
	b3G(state, 2, 6, 10, 14, message[3], message[4])
	b3G(state, 3, 7, 11, 15, message[5], message[6])
	b3G(state, 4, 8, 12, 16, message[7], message[8])
	b3G(state, 1, 6, 11, 16, message[9], message[10])
	b3G(state, 2, 7, 12, 13, message[11], message[12])
	b3G(state, 3, 8, 9, 14, message[13], message[14])
	b3G(state, 4, 5, 10, 15, message[15], message[16])
end

local function b3Permute(message: { number }): { number }
	local result = table.create(16)
	for index = 1, 16 do
		result[index] = message[B3_PERMUTATION[index]]
	end
	return result
end

local function b3BlockWords(block: string): { number }
	local words = table.create(16, 0)
	for wordIndex = 0, 15 do
		local offset = wordIndex * 4
		local b0 = if offset + 1 <= #block then string.byte(block, offset + 1) else 0
		local b1 = if offset + 2 <= #block then string.byte(block, offset + 2) else 0
		local b2 = if offset + 3 <= #block then string.byte(block, offset + 3) else 0
		local b3 = if offset + 4 <= #block then string.byte(block, offset + 4) else 0
		words[wordIndex + 1] = b0 + b1 * 0x100 + b2 * 0x10000 + b3 * 0x1000000
	end
	return words
end

type B3Output = {
	inputCv: { number },
	blockWords: { number },
	counter: number,
	blockLength: number,
	flags: number,
}

local function b3Compress(output: B3Output, extraFlags: number?): { number }
	local state = table.create(16)
	for index = 1, 8 do
		state[index] = output.inputCv[index]
	end
	for index = 1, 4 do
		state[index + 8] = B3_IV[index]
	end
	state[13] = output.counter % 0x100000000
	state[14] = math.floor(output.counter / 0x100000000)
	state[15] = output.blockLength
	state[16] = bit32.bor(output.flags, extraFlags or 0)
	local schedule = copyWords(output.blockWords)
	for round = 1, 7 do
		b3Round(state, schedule)
		if round < 7 then
			schedule = b3Permute(schedule)
		end
	end
	local result = table.create(16)
	for index = 1, 8 do
		result[index] = bit32.bxor(state[index], state[index + 8])
		result[index + 8] = bit32.bxor(state[index + 8], output.inputCv[index])
	end
	return result
end

local function b3OutputCv(output: B3Output): { number }
	local compressed = b3Compress(output)
	local result = table.create(8)
	for index = 1, 8 do
		result[index] = compressed[index]
	end
	return result
end

local function b3ChunkOutput(chunk: string, counter: number): B3Output
	local blockCount = math.max(1, math.ceil(#chunk / 64))
	local cv = copyWords(B3_IV)
	for blockIndex = 0, blockCount - 1 do
		local block = string.sub(chunk, blockIndex * 64 + 1, math.min(#chunk, (blockIndex + 1) * 64))
		local flags = 0
		if blockIndex == 0 then
			flags = bit32.bor(flags, B3_CHUNK_START)
		end
		if blockIndex == blockCount - 1 then
			flags = bit32.bor(flags, B3_CHUNK_END)
		end
		local output: B3Output = {
			inputCv = copyWords(cv),
			blockWords = b3BlockWords(block),
			counter = counter,
			blockLength = #block,
			flags = flags,
		}
		if blockIndex == blockCount - 1 then
			return output
		end
		cv = b3OutputCv(output)
	end
	error("unreachable")
end

local function b3ParentOutput(left: { number }, right: { number }): B3Output
	local block = table.create(16)
	for index = 1, 8 do
		block[index] = left[index]
		block[index + 8] = right[index]
	end
	return { inputCv = copyWords(B3_IV), blockWords = block, counter = 0, blockLength = 64, flags = B3_PARENT }
end

local function wordToLittleEndian(word: number): string
	return string.char(
		word % 0x100,
		math.floor(word / 0x100) % 0x100,
		math.floor(word / 0x10000) % 0x100,
		math.floor(word / 0x1000000) % 0x100
	)
end

local function bytesToBase32(bytes: string): string
	local alphabet = "abcdefghijklmnopqrstuvwxyz234567"
	local output = table.create(math.ceil(#bytes * 8 / 5))
	local accumulator = 0
	local bits = 0
	for index = 1, #bytes do
		accumulator = accumulator * 0x100 + string.byte(bytes, index)
		bits += 8
		while bits >= 5 do
			bits -= 5
			local digit = math.floor(accumulator / (2 ^ bits)) % 32
			table.insert(output, string.sub(alphabet, digit + 1, digit + 1))
		end
		if bits == 0 then
			accumulator = 0
		else
			accumulator %= 2 ^ bits
		end
	end
	if bits > 0 then
		local digit = (accumulator * (2 ^ (5 - bits))) % 32
		table.insert(output, string.sub(alphabet, digit + 1, digit + 1))
	end
	return table.concat(output)
end

local function blake3Bytes(input: string): string
	local chunkCount = math.max(1, math.ceil(#input / 1024))
	local cvStack: { { number } } = {}
	for chunkIndex = 0, chunkCount - 2 do
		local chunk = string.sub(input, chunkIndex * 1024 + 1, (chunkIndex + 1) * 1024)
		local cv = b3OutputCv(b3ChunkOutput(chunk, chunkIndex))
		local totalChunks = chunkIndex + 1
		while totalChunks % 2 == 0 do
			local left = table.remove(cvStack)
			assert(left ~= nil, "BLAKE3 tree underflow")
			cv = b3OutputCv(b3ParentOutput(left :: { number }, cv))
			totalChunks = math.floor(totalChunks / 2)
		end
		table.insert(cvStack, cv)
	end
	local lastIndex = chunkCount - 1
	local finalChunk = string.sub(input, lastIndex * 1024 + 1, math.min(#input, (lastIndex + 1) * 1024))
	local output = b3ChunkOutput(finalChunk, lastIndex)
	for index = #cvStack, 1, -1 do
		output = b3ParentOutput(cvStack[index], b3OutputCv(output))
	end
	local rootWords = b3Compress(output, B3_ROOT)
	local pieces = table.create(8)
	for index = 1, 8 do
		pieces[index] = wordToLittleEndian(rootWords[index])
	end
	return table.concat(pieces)
end

local function blake3Label(input: string): string
	return "b3-256:" .. bytesToBase32(blake3Bytes(input))
end

local BLAKE3_SELF_TEST = blake3Label("") == "b3-256:v4jutopv7gq2nicajxvdnxgjjgn4wjojvxarfn6mtkj4vza7gjra"
	and blake3Label("abc") == "b3-256:mq33hlbyizith75whn2soounwvemkwcglv45wa75gwogzvn5twcq"

-- Settings and compatibility -------------------------------------------------

local Settings = {}
Settings.__index = Settings

function Settings.new(pluginObject: Plugin)
	return setmetatable({ plugin = pluginObject, prefix = "NeuMan.v1." }, Settings)
end

function Settings:get(key: string, fallback: any): any
	local ok, value = pcall(function()
		return self.plugin:GetSetting(self.prefix .. key)
	end)
	if ok and value ~= nil then
		return value
	end
	return fallback
end

function Settings:set(key: string, value: any)
	local ok, problem = pcall(function()
		self.plugin:SetSetting(self.prefix .. key, value)
	end)
	if not ok then
		warn("NeuMan could not store non-cloud setting " .. key .. ": " .. tostring(problem))
	end
end

local settings = Settings.new(plugin)
local pluginInstallationId = settings:get("PluginInstallationId", nil)
if type(pluginInstallationId) ~= "string" or not string.match(pluginInstallationId, "^plg_[%w%-_]+$") then
	pluginInstallationId = "plg_" .. string.lower(HttpService:GenerateGUID(false))
	settings:set("PluginInstallationId", pluginInstallationId)
end

local serializationOk, SerializationService = pcall(function()
	return game:GetService("SerializationService")
end)
local webStreamAvailable = type((HttpService :: any).CreateWebStreamClient) == "function"
local compatibilityProblems: { string } = {}
if not serializationOk then
	table.insert(compatibilityProblems, "SerializationService is unavailable")
end
if not webStreamAvailable then
	table.insert(compatibilityProblems, "HttpService WebStream client is unavailable")
end
if not BLAKE3_SELF_TEST then
	table.insert(compatibilityProblems, "native hash self-test failed")
end

-- Cell registry and change tracking -----------------------------------------

local function isCellId(value: any): boolean
	return type(value) == "string"
		and string.match(value, "^cell_[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+%-[0-9a-f]+$") ~= nil
end

local function isForbiddenContent(instance: Instance): boolean
	return instance:IsA("LuaSourceContainer") or instance:IsA("RemoteEvent") or instance:IsA("RemoteFunction")
end

local function scanCells(): (JsonMap, { string })
	local byId: JsonMap = {}
	local problems: { string } = {}
	for _, candidate in game:GetDescendants() do
		local id = candidate:GetAttribute(CELL_ATTRIBUTE)
		if id ~= nil then
			if not isCellId(id) then
				table.insert(problems, "Malformed cell ID at " .. candidate:GetFullName())
			elseif byId[id] ~= nil then
				table.insert(problems, "Duplicate cell ID " .. id)
			else
				byId[id] = candidate
			end
		end
	end
	return byId, problems
end

local ChangeTracker = {
	epochs = {} :: JsonMap,
	dirty = {} :: JsonMap,
	connections = {} :: JsonMap,
	suppressed = false,
	onChanged = Instance.new("BindableEvent"),
}

function ChangeTracker:mark(root: Instance, reason: string)
	if self.suppressed then
		return
	end
	local id = root:GetAttribute(CELL_ATTRIBUTE)
	if not isCellId(id) then
		return
	end
	self.epochs[id] = (self.epochs[id] or 0) + 1
	self.dirty[id] = reason
	self.onChanged:Fire(id, true, reason, self.epochs[id])
end

function ChangeTracker:watch(root: Instance)
	local id = root:GetAttribute(CELL_ATTRIBUTE)
	if not isCellId(id) or self.connections[id] ~= nil then
		return
	end
	self.epochs[id] = self.epochs[id] or 0
	local connections: ConnectionList = {}
	local function watchDescendant(descendant: Instance)
		table.insert(
			connections,
			descendant.Changed:Connect(function()
				self:mark(root, "property")
			end)
		)
		table.insert(
			connections,
			descendant.AttributeChanged:Connect(function()
				self:mark(root, "attribute")
			end)
		)
	end
	watchDescendant(root)
	for index, descendant in root:GetDescendants() do
		watchDescendant(descendant)
		if index % 250 == 0 then
			task.wait()
		end
	end
	table.insert(
		connections,
		root.DescendantAdded:Connect(function(descendant)
			watchDescendant(descendant)
			self:mark(root, "descendant-added")
		end)
	)
	table.insert(
		connections,
		root.DescendantRemoving:Connect(function()
			self:mark(root, "descendant-removing")
		end)
	)
	table.insert(
		connections,
		root.AncestryChanged:Connect(function()
			self:mark(root, "ancestry")
		end)
	)
	self.connections[id] = connections
end

function ChangeTracker:unwatch(cellId: string)
	local connections = self.connections[cellId]
	if connections == nil then
		return
	end
	for _, connection in connections do
		connection:Disconnect()
	end
	self.connections[cellId] = nil
end

function ChangeTracker:refresh()
	local cells = scanCells()
	for _, root in cells do
		self:watch(root)
	end
end

function ChangeTracker:isDirty(cellId: string): boolean
	return self.dirty[cellId] ~= nil
end

function ChangeTracker:accept(cellId: string)
	self.dirty[cellId] = nil
	self.onChanged:Fire(cellId, false, "accepted", self.epochs[cellId] or 0)
end

ChangeTracker:refresh()

-- Widget --------------------------------------------------------------------

local toolbar = plugin:CreateToolbar("NeuMan")
local openButton = toolbar:CreateButton("Open NeuMan", "Open the NeuMan build manager", "rbxassetid://0")
local checkpointButton = toolbar:CreateButton("Checkpoint", "Checkpoint the selected art cell", "rbxassetid://0")
local applyButton =
	toolbar:CreateButton("Apply Incoming", "Preview and apply accepted art without reopening Studio", "rbxassetid://0")

local widgetInfo = DockWidgetPluginGuiInfo.new(Enum.InitialDockState.Right, false, false, 390, 620, 320, 420)
local widget = plugin:CreateDockWidgetPluginGui("NeuManStudioBridge", widgetInfo)
widget.Title = "NeuMan Studio"

local colors = {
	background = Color3.fromRGB(18, 20, 25),
	panel = Color3.fromRGB(27, 30, 37),
	text = Color3.fromRGB(237, 239, 244),
	muted = Color3.fromRGB(155, 164, 180),
	accent = Color3.fromRGB(90, 130, 255),
	success = Color3.fromRGB(70, 205, 140),
	warning = Color3.fromRGB(247, 181, 69),
	danger = Color3.fromRGB(242, 91, 102),
}

local rootFrame = Instance.new("ScrollingFrame")
rootFrame.Name = "Root"
rootFrame.BackgroundColor3 = colors.background
rootFrame.BorderSizePixel = 0
rootFrame.Size = UDim2.fromScale(1, 1)
rootFrame.AutomaticCanvasSize = Enum.AutomaticSize.Y
rootFrame.CanvasSize = UDim2.new()
rootFrame.ScrollBarThickness = 5
rootFrame.Parent = widget

local rootPadding = Instance.new("UIPadding")
rootPadding.PaddingTop = UDim.new(0, 16)
rootPadding.PaddingBottom = UDim.new(0, 16)
rootPadding.PaddingLeft = UDim.new(0, 16)
rootPadding.PaddingRight = UDim.new(0, 16)
rootPadding.Parent = rootFrame
local rootLayout = Instance.new("UIListLayout")
rootLayout.Padding = UDim.new(0, 10)
rootLayout.SortOrder = Enum.SortOrder.LayoutOrder
rootLayout.Parent = rootFrame

local function label(text: string, size: number, color: Color3?): TextLabel
	local result = Instance.new("TextLabel")
	result.BackgroundTransparency = 1
	result.Size = UDim2.new(1, 0, 0, size + 8)
	result.Font = Enum.Font.BuilderSans
	result.Text = text
	result.TextColor3 = color or colors.text
	result.TextSize = size
	result.TextXAlignment = Enum.TextXAlignment.Left
	result.TextWrapped = true
	result.AutomaticSize = Enum.AutomaticSize.Y
	return result
end

local function button(text: string): TextButton
	local result = Instance.new("TextButton")
	result.AutoButtonColor = true
	result.BackgroundColor3 = colors.accent
	result.BorderSizePixel = 0
	result.Size = UDim2.new(1, 0, 0, 36)
	result.Font = Enum.Font.BuilderSansBold
	result.Text = text
	result.TextColor3 = Color3.new(1, 1, 1)
	result.TextSize = 14
	local corner = Instance.new("UICorner")
	corner.CornerRadius = UDim.new(0, 7)
	corner.Parent = result
	return result
end

local function textBox(placeholder: string, initial: string): TextBox
	local result = Instance.new("TextBox")
	result.BackgroundColor3 = colors.panel
	result.BorderSizePixel = 0
	result.ClearTextOnFocus = false
	result.PlaceholderText = placeholder
	result.Text = initial
	result.TextColor3 = colors.text
	result.PlaceholderColor3 = colors.muted
	result.Font = Enum.Font.BuilderSans
	result.TextSize = 14
	result.Size = UDim2.new(1, 0, 0, 34)
	result.TextXAlignment = Enum.TextXAlignment.Left
	local padding = Instance.new("UIPadding")
	padding.PaddingLeft = UDim.new(0, 10)
	padding.PaddingRight = UDim.new(0, 10)
	padding.Parent = result
	local corner = Instance.new("UICorner")
	corner.CornerRadius = UDim.new(0, 7)
	corner.Parent = result
	return result
end

local heading = label("NeuMan  •  Studio authoring", 18)
heading.Font = Enum.Font.BuilderSansBold
heading.Parent = rootFrame
local statusLabel = label("Disconnected", 14, colors.warning)
statusLabel.Parent = rootFrame
local detailLabel = label("Start the NeuMan desktop app, then connect.", 12, colors.muted)
detailLabel.Parent = rootFrame

local projectBox = textBox("Project ID", settings:get("ProjectId", Workspace:GetAttribute(PROJECT_ATTRIBUTE) or ""))
projectBox.Parent = rootFrame
local channelBox = textBox("Authoring channel", settings:get("Channel", "art/main"))
channelBox.Parent = rootFrame
local saveIdentityButton = button("Save project identity")
saveIdentityButton.BackgroundColor3 = colors.panel
saveIdentityButton.Parent = rootFrame

local connectButton = button("Connect to NeuMan")
connectButton.Parent = rootFrame
local pairingBox = textBox("Six-digit code shown by desktop", "")
pairingBox.Visible = false
pairingBox.Parent = rootFrame
local pairButton = button("Pair this Studio installation")
pairButton.Visible = false
pairButton.Parent = rootFrame

local selectionLabel = label("Selection: no managed cell", 13, colors.muted)
selectionLabel.Parent = rootFrame
local registerButton = button("Register selection as art cell")
registerButton.BackgroundColor3 = colors.panel
registerButton.Parent = rootFrame
local checkpointWidgetButton = button("Checkpoint selected cell")
checkpointWidgetButton.Parent = rootFrame

local incomingLabel = label("Incoming: none", 13, colors.muted)
incomingLabel.Parent = rootFrame
local applyWidgetButton = button("Preview incoming revision")
applyWidgetButton.BackgroundColor3 = colors.panel
applyWidgetButton.Parent = rootFrame

local limitations = label(
	"Safety: edit mode only; scripts/remotes are rejected; dirty cells never auto-apply. "
		.. "Terrain and service-state adapters are not enabled in this build.",
	11,
	colors.muted
)
limitations.Parent = rootFrame

local function setStatus(status: string, details: string, color: Color3?)
	statusLabel.Text = status
	statusLabel.TextColor3 = color or colors.text
	detailLabel.Text = details
end

-- Bridge client --------------------------------------------------------------

local BridgeClient = {
	stream = nil :: any,
	streamConnections = {} :: ConnectionList,
	credential = settings:get("PairCredential", nil),
	discovery = nil :: JsonMap?,
	sessionId = nil :: string?,
	sequence = 1,
	serverSequence = 1,
	context = nil :: JsonMap?,
	pending = {} :: JsonMap,
	connected = false,
	closing = false,
	onIncoming = Instance.new("BindableEvent"),
	onCommand = Instance.new("BindableEvent"),
}

local function isNumericLoopbackUrl(url: any, scheme: string): boolean
	if type(url) ~= "string" or #url > 256 then
		return false
	end
	if string.find(url, "@", 1, true) or string.find(url, "?", 1, true) or string.find(url, "#", 1, true) then
		return false
	end
	local prefix = scheme .. "://127.0.0.1:"
	if string.sub(url, 1, #prefix) ~= prefix then
		return false
	end
	return string.match(string.sub(url, #prefix + 1), "^%d+/[%w%-%._/]+$") ~= nil
end

local function messageText(message: any): string?
	if type(message) == "string" then
		return message
	elseif type(message) == "table" and type(message.Data) == "string" then
		return message.Data
	end
	return nil
end

function BridgeClient:discover(): (boolean, string?)
	local ok, response = pcall(function()
		return HttpService:RequestAsync({
			Url = DISCOVERY_URL,
			Method = "GET",
			Headers = { Accept = "application/json" },
		})
	end)
	if not ok then
		return false, "Discovery failed. Enable HTTP requests and start NeuMan."
	end
	if not response.Success or #response.Body > 16 * 1024 then
		return false, "The local bridge returned an invalid discovery response."
	end
	local decodedOk, document = pcall(function()
		return HttpService:JSONDecode(response.Body)
	end)
	if not decodedOk or type(document) ~= "table" or document.schemaVersion ~= PROTOCOL_VERSION then
		return false, "The local bridge protocol is incompatible."
	end
	if not isNumericLoopbackUrl(document.webSocketUrl, "ws") then
		return false, "The bridge advertised a non-loopback endpoint; connection was refused."
	end
	if type(document.challenge) ~= "string" or #document.challenge < 22 then
		return false, "The bridge pairing challenge is invalid."
	end
	self.discovery = document
	return true, nil
end

function BridgeClient:rawSend(payload: JsonMap)
	if self.stream == nil then
		error("bridge is not connected")
	end
	local encoded = HttpService:JSONEncode(payload)
	if #encoded > 1024 * 1024 then
		error("control message exceeds 1 MiB")
	end
	local stream: any = self.stream
	stream:Send(encoded)
end

function BridgeClient:sendEnvelope(kind: string, payload: JsonMap, correlationId: string?): string
	if not self.connected or self.sessionId == nil then
		error("bridge session is not authenticated")
	end
	local messageId = "msg_" .. string.lower(HttpService:GenerateGUID(false))
	self:rawSend({
		protocolVersion = PROTOCOL_VERSION,
		type = kind,
		messageId = messageId,
		correlationId = correlationId,
		sequence = self.sequence,
		sentAt = DateTime.now():ToIsoDate(),
		sessionId = self.sessionId,
		payload = payload,
	})
	self.sequence += 1
	return messageId
end

function BridgeClient:request(
	kind: string,
	payload: JsonMap,
	expectedKind: string,
	timeoutSeconds: number
): (JsonMap?, string?)
	local event = Instance.new("BindableEvent")
	local messageId = "msg_" .. string.lower(HttpService:GenerateGUID(false))
	self.pending[messageId] = { event = event, expected = expectedKind }
	local sent, sendProblem = pcall(function()
		self:rawSend({
			protocolVersion = PROTOCOL_VERSION,
			type = kind,
			messageId = messageId,
			correlationId = nil,
			sequence = self.sequence,
			sentAt = DateTime.now():ToIsoDate(),
			sessionId = self.sessionId,
			payload = payload,
		})
	end)
	if not sent then
		self.pending[messageId] = nil
		event:Destroy()
		return nil, "Bridge send failed: " .. tostring(sendProblem)
	end
	self.sequence += 1
	task.delay(timeoutSeconds, function()
		local pending = self.pending[messageId]
		if pending ~= nil then
			self.pending[messageId] = nil
			pending.event:Fire(nil, "Timed out waiting for " .. expectedKind)
		end
	end)
	local response, problem = event.Event:Wait()
	event:Destroy()
	return response, problem
end

function BridgeClient:hello()
	local userId: string? = nil
	pcall(function()
		userId = tostring((StudioService :: any):GetUserId())
	end)
	self:sendEnvelope("session.hello", {
		pluginVersion = PLUGIN_VERSION,
		pluginInstallationId = pluginInstallationId,
		studioVersion = version(),
		studioUserId = userId,
		universeId = if game.GameId > 0 then tostring(game.GameId) else nil,
		placeId = if game.PlaceId > 0 then tostring(game.PlaceId) else nil,
		dataModelName = game.Name,
		runState = if RunService:IsRunning() then "play" else "edit",
		projectId = projectBox.Text,
		channel = channelBox.Text,
		capabilities = {
			"base64-chunks",
			"semantic-fingerprint-v1",
			"apply-transaction-v1",
			"http-download-v1",
		},
	})
end

function BridgeClient:onMessage(rawMessage: any)
	local raw = messageText(rawMessage)
	if raw == nil or #raw > 1024 * 1024 then
		self:disconnect("invalid or oversized bridge message")
		return
	end
	local ok, message = pcall(function()
		return HttpService:JSONDecode(raw :: string)
	end)
	if not ok or type(message) ~= "table" or type(message.type) ~= "string" then
		self:disconnect("malformed bridge message")
		return
	end
	if message.type == "session.authenticated" then
		if type(message.sessionId) ~= "string" or message.protocolVersion ~= PROTOCOL_VERSION then
			self:disconnect("authentication response was invalid")
			return
		end
		self.sessionId = message.sessionId
		self.sequence = 1
		self.serverSequence = 1
		self.connected = true
		setStatus("Connected • validating place", "Waiting for an exact project/place binding.", colors.warning)
		self:hello()
		return
	end
	if message.protocolVersion ~= PROTOCOL_VERSION or message.sessionId ~= self.sessionId then
		self:disconnect("session binding mismatch")
		return
	end
	if type(message.sequence) ~= "number" or message.sequence ~= self.serverSequence then
		self:disconnect("server sequence gap or replay detected")
		return
	end
	self.serverSequence += 1
	local correlation = message.correlationId
	if type(correlation) == "string" and self.pending[correlation] ~= nil then
		local pending = self.pending[correlation]
		self.pending[correlation] = nil
		if message.type == pending.expected then
			pending.event:Fire(message.payload, nil)
		else
			pending.event:Fire(nil, "Expected " .. pending.expected .. ", received " .. message.type)
		end
		return
	end
	if message.type == "session.ping" then
		self:sendEnvelope("session.pong", { receivedAt = DateTime.now():ToIsoDate() }, message.messageId)
	elseif message.type == "session.context" or message.type == "session.context-changed" then
		if message.type == "session.context-changed" and message.payload.requiresRefresh == true then
			self.context = nil
			setStatus(
				"Context changed • revalidating",
				"Mutation is paused until project/place identity is checked again.",
				colors.warning
			)
			self:hello()
			return
		end
		self.context = message.payload
		if message.payload.bound == true then
			local binding = message.payload.binding or {}
			setStatus(
				"Connected • " .. tostring(binding.channel or channelBox.Text),
				"Project " .. tostring(binding.projectId or projectBox.Text) .. " • Place " .. tostring(game.PlaceId),
				colors.success
			)
		else
			setStatus(
				"Connected • mutation blocked",
				"Project/place/channel identity does not match the daemon.",
				colors.danger
			)
		end
	elseif message.type == "art.incoming-summary" then
		self.onIncoming:Fire(message.payload)
	elseif message.type == "command.request" then
		self.onCommand:Fire(message.payload)
	elseif message.type == "protocol.error" then
		local problem = message.payload or {}
		setStatus("Bridge error", tostring(problem.code or "unknown protocol error"), colors.danger)
	end
end

function BridgeClient:openStream(headers: JsonMap): (boolean, string?)
	if self.discovery == nil then
		return false, "Discovery has not completed."
	end
	local ok, stream = pcall(function()
		return (HttpService :: any):CreateWebStreamClient(Enum.WebStreamClientType.WebSocket, {
			Url = (self.discovery :: JsonMap).webSocketUrl,
			Method = "GET",
			Headers = headers,
		})
	end)
	if not ok then
		return false, "Studio could not create the local WebSocket stream."
	end
	self.stream = stream
	self.closing = false
	local streamAny: any = stream
	local openedEvent = Instance.new("BindableEvent")
	local opening = true
	local function resolveOpen(success: boolean, problem: string)
		if opening then
			opening = false
			openedEvent:Fire(success, problem)
		end
	end
	table.insert(
		self.streamConnections,
		streamAny.MessageReceived:Connect(function(message)
			self:onMessage(message)
		end)
	)
	table.insert(
		self.streamConnections,
		streamAny.Opened:Connect(function(statusCode: number)
			resolveOpen(statusCode >= 200 and statusCode < 300, "")
		end)
	)
	table.insert(
		self.streamConnections,
		streamAny.Error:Connect(function(statusCode: number, errorMessage: string)
			resolveOpen(false, string.format("WebSocket error %d: %s", statusCode, errorMessage))
		end)
	)
	table.insert(
		self.streamConnections,
		streamAny.Closed:Connect(function()
			if not self.closing then
				self.connected = false
				setStatus("Disconnected", "The local bridge closed. Reconnect to revalidate context.", colors.warning)
			end
		end)
	)
	task.delay(5, function()
		resolveOpen(false, "Timed out opening the local WebSocket.")
	end)
	local opened, openingProblem = openedEvent.Event:Wait()
	openedEvent:Destroy()
	if not opened then
		self:disconnect(openingProblem)
		return false, openingProblem
	end
	return true, nil
end

function BridgeClient:connect(): (boolean, string?)
	if #compatibilityProblems > 0 then
		return false, table.concat(compatibilityProblems, "; ")
	end
	local discovered, problem = self:discover()
	if not discovered then
		return false, problem
	end
	if type(self.credential) ~= "string" then
		pairingBox.Visible = true
		pairButton.Visible = true
		return false, "Pairing is required. Enter the code displayed by the NeuMan desktop."
	end
	local opened, openProblem = self:openStream({
		Authorization = "NeuManPair " .. self.credential,
		["X-NeuMan-Plugin-Installation"] = pluginInstallationId,
		["X-NeuMan-Protocol"] = PROTOCOL_VERSION,
	})
	if not opened then
		return false, openProblem
	end
	-- Header auth is preferred; fallback auth is harmless if the server already
	-- authenticated headers only when it asks for a first message.
	task.delay(0.25, function()
		if not self.connected and self.stream ~= nil then
			pcall(function()
				self:rawSend({
					type = "session.authenticate",
					credential = self.credential,
					pluginInstallationId = pluginInstallationId,
					protocolMin = PROTOCOL_VERSION,
					protocolMax = PROTOCOL_VERSION,
				})
			end)
		end
	end)
	return true, nil
end

function BridgeClient:pair(code: string): (boolean, string?)
	if not string.match(code, "^%d%d%d%d%d%d$") then
		return false, "Enter the exact six-digit desktop code."
	end
	local discovered, problem = self:discover()
	if not discovered then
		return false, problem
	end
	local responseEvent = Instance.new("BindableEvent")
	local pairingWaiting = true
	local function resolvePair(response: JsonMap)
		if pairingWaiting then
			pairingWaiting = false
			responseEvent:Fire(response)
		end
	end
	local opened, openProblem = self:openStream({})
	if not opened then
		responseEvent:Destroy()
		return false, openProblem
	end
	local oldHandler = self.onMessage
	self.onMessage = function(_self: any, rawMessage: any)
		local raw = messageText(rawMessage)
		local ok, response = pcall(function()
			return HttpService:JSONDecode(raw or "")
		end)
		if ok and type(response) == "table" and string.sub(tostring(response.type), 1, 5) == "pair." then
			resolvePair(response)
		else
			oldHandler(_self, rawMessage)
		end
	end
	local sent, sendProblem = pcall(function()
		self:rawSend({
			type = "pair.request",
			requestId = "msg_" .. string.lower(HttpService:GenerateGUID(false)),
			protocolVersion = PROTOCOL_VERSION,
			challenge = (self.discovery :: JsonMap).challenge,
			pairingCode = code,
			pluginInstallationId = pluginInstallationId,
			pluginVersion = PLUGIN_VERSION,
			studio = { userId = nil, version = version(), platform = "unknown" },
		})
	end)
	if not sent then
		responseEvent:Destroy()
		self.onMessage = oldHandler
		self:disconnect("pairing send failed")
		return false, "Pairing send failed: " .. tostring(sendProblem)
	end
	task.delay(8, function()
		resolvePair({ type = "pair.timeout" })
	end)
	local response = responseEvent.Event:Wait()
	responseEvent:Destroy()
	self.onMessage = oldHandler
	self:disconnect("pairing complete")
	if response.type == "pair.succeeded" and type(response.credential) == "string" then
		self.credential = response.credential
		settings:set("PairCredential", response.credential)
		pairingBox.Text = ""
		pairingBox.Visible = false
		pairButton.Visible = false
		return true, nil
	elseif response.type == "pair.pending" then
		return false, "Approve this Studio installation in the desktop, then click Pair again."
	end
	return false, "Pairing was denied or timed out. Request a new desktop code."
end

function BridgeClient:disconnect(reason: string)
	self.closing = true
	self.connected = false
	self.context = nil
	if self.stream ~= nil then
		pcall(function()
			local stream: any = self.stream
			stream:Close()
		end)
	end
	self.stream = nil
	for _, connection in self.streamConnections do
		connection:Disconnect()
	end
	table.clear(self.streamConnections)
	self.sessionId = nil
	if reason ~= "pairing complete" then
		setStatus("Disconnected", reason, colors.warning)
	end
end

function BridgeClient:isBound(): boolean
	return self.connected and self.context ~= nil and (self.context :: JsonMap).bound == true
end

function BridgeClient:contextVersion(): number?
	if not self:isBound() then
		return nil
	end
	local binding = (self.context :: JsonMap).binding
	return if type(binding) == "table" then binding.contextVersion else nil
end

function BridgeClient:acceptedArtHead(): string?
	if not self:isBound() then
		return nil
	end
	local binding = (self.context :: JsonMap).binding
	local head = if type(binding) == "table" then binding.acceptedArtHead else nil
	return if type(head) == "string" and string.sub(head, 1, 4) == "art_" then head else nil
end

function BridgeClient:recordAcceptedArtHead(revisionId: string, stateRoot: any)
	if not self:isBound() then
		return
	end
	local binding = (self.context :: JsonMap).binding
	if type(binding) == "table" then
		binding.acceptedArtHead = revisionId
		if type(stateRoot) == "string" then
			binding.acceptedStateRoot = stateRoot
		end
	end
end

-- Native capture -------------------------------------------------------------

local function selectedCell(): Instance?
	local selected = Selection:Get()
	if #selected ~= 1 then
		return nil
	end
	local cursor: Instance? = selected[1]
	while cursor ~= nil and cursor ~= game do
		if isCellId(cursor:GetAttribute(CELL_ATTRIBUTE)) then
			return cursor
		end
		cursor = cursor.Parent
	end
	return nil
end

local function validateCell(root: Instance): (boolean, string?)
	if not isCellId(root:GetAttribute(CELL_ATTRIBUTE)) then
		return false, "The selected root does not have a valid NeuMan cell ID."
	end
	local _, problems = scanCells()
	if #problems > 0 then
		return false, table.concat(problems, "; ")
	end
	for _, descendant in root:GetDescendants() do
		if isForbiddenContent(descendant) then
			return false, "Forbidden executable/network content: " .. descendant:GetFullName()
		end
		local nestedId = descendant:GetAttribute(CELL_ATTRIBUTE)
		if nestedId ~= nil then
			return false, "Nested art cells are not supported: " .. descendant:GetFullName()
		end
	end
	return true, nil
end

local function base64UrlEncode(bytes: string): string
	local encoded = HttpService:Base64Encode(bytes)
	encoded = string.gsub(encoded, "+", "-")
	encoded = string.gsub(encoded, "/", "_")
	encoded = string.gsub(encoded, "=+$", "")
	return encoded
end

local function base64UrlDecode(encoded: string): string
	local standard = string.gsub(string.gsub(encoded, "-", "+"), "_", "/")
	local remainder = #standard % 4
	if remainder > 0 then
		standard ..= string.rep("=", 4 - remainder)
	end
	return HttpService:Base64Decode(standard)
end

local function captureSlotPath(instance: Instance?): string?
	if instance == nil or instance == game then
		return nil
	end
	local parts = {}
	local cursor: Instance? = instance
	while cursor ~= nil and cursor ~= game do
		local segment = string.gsub(cursor.Name, "~", "~0")
		segment = string.gsub(segment, "/", "~1")
		table.insert(parts, 1, segment)
		cursor = cursor.Parent
	end
	if cursor ~= game or #parts == 0 then
		return nil
	end
	return "/" .. table.concat(parts, "/")
end

local captureBusy = false
local function checkpointSelected()
	if captureBusy then
		setStatus("Checkpoint busy", "Only one Studio serialization may run at a time.", colors.warning)
		return
	end
	if RunService:IsRunning() then
		setStatus("Checkpoint blocked", "Stop play/test mode before capturing art.", colors.danger)
		return
	end
	if not BridgeClient:isBound() then
		setStatus("Checkpoint blocked", "Connect with an exact project/place/channel binding first.", colors.danger)
		return
	end
	local root = selectedCell()
	if root == nil then
		setStatus("Checkpoint blocked", "Select exactly one managed art cell.", colors.danger)
		return
	end
	local valid, validationProblem = validateCell(root)
	if not valid then
		setStatus("Checkpoint blocked", validationProblem or "cell validation failed", colors.danger)
		return
	end
	captureBusy = true
	setStatus("Serializing cell", root:GetFullName(), colors.warning)
	task.spawn(function()
		local cellId = root:GetAttribute(CELL_ATTRIBUTE) :: string
		local bytes: string? = nil
		local stableEpoch = 0
		local problem: string? = nil
		for _attempt = 1, 3 do
			local epoch0 = ChangeTracker.epochs[cellId] or 0
			local ok, serialized = pcall(function()
				return (SerializationService :: any):SerializeInstancesAsync({ root })
			end)
			local epoch1 = ChangeTracker.epochs[cellId] or 0
			if not ok then
				problem = "SerializationService failed: " .. tostring(serialized)
				break
			elseif epoch0 == epoch1 then
				if typeof(serialized) == "buffer" then
					bytes = buffer.tostring(serialized)
				elseif type(serialized) == "string" then
					bytes = serialized
				end
				stableEpoch = epoch1
				break
			end
			task.wait(0.25)
		end
		if bytes == nil then
			captureBusy = false
			setStatus(
				"Checkpoint failed",
				problem or "Cell kept changing during capture; try again when edits settle.",
				colors.danger
			)
			return
		end
		if #bytes > MAX_CELL_BYTES then
			captureBusy = false
			setStatus(
				"Checkpoint blocked",
				"Cell exceeds the 96 MiB hard limit; split it into smaller cells.",
				colors.danger
			)
			return
		end
		local transferId = "op_" .. string.lower(HttpService:GenerateGUID(false))
		local contextVersion = BridgeClient:contextVersion()
		local chunkCount = math.ceil(#bytes / CHUNK_BYTES)
		setStatus("Uploading checkpoint", string.format("%s • %.1f MiB", cellId, #bytes / 1048576), colors.warning)
		local accepted, offerProblem = BridgeClient:request("transfer.offer", {
			contextVersion = contextVersion,
			transferId = transferId,
			direction = "upload",
			purpose = "art-cell",
			resourceId = cellId,
			contentHash = blake3Label(bytes),
			mediaType = "application/x-roblox-rbxm",
			sizeBytes = #bytes,
			chunkSizeBytes = CHUNK_BYTES,
			chunkCount = chunkCount,
			encoding = "base64url",
			compression = "none",
		}, "transfer.accept", 10)
		if accepted == nil then
			captureBusy = false
			setStatus("Checkpoint rejected", offerProblem or "transfer offer rejected", colors.danger)
			return
		end
		for chunkIndex = 0, chunkCount - 1 do
			local offset = chunkIndex * CHUNK_BYTES
			local chunk = string.sub(bytes, offset + 1, math.min(#bytes, offset + CHUNK_BYTES))
			local _, chunkProblem = BridgeClient:request("transfer.chunk", {
				contextVersion = contextVersion,
				transferId = transferId,
				chunkIndex = chunkIndex,
				offsetBytes = offset,
				rawSizeBytes = #chunk,
				chunkHash = blake3Label(chunk),
				data = base64UrlEncode(chunk),
			}, "transfer.ack", 15)
			if chunkProblem ~= nil then
				BridgeClient:sendEnvelope(
					"transfer.cancel",
					{ contextVersion = contextVersion, transferId = transferId }
				)
				captureBusy = false
				setStatus("Checkpoint failed", chunkProblem, colors.danger)
				return
			end
		end
		local verified, verifyProblem = BridgeClient:request(
			"transfer.complete",
			{ contextVersion = contextVersion, transferId = transferId },
			"transfer.verified",
			30
		)
		if verified == nil then
			captureBusy = false
			setStatus("Checkpoint failed", verifyProblem or "content verification failed", colors.danger)
			return
		end
		local baseRevision = root:GetAttribute("NeuManBaseRevision")
		if type(baseRevision) ~= "string" then
			baseRevision = BridgeClient:acceptedArtHead()
		end
		local proposal, proposalProblem = BridgeClient:request("art.capture.proposal", {
			contextVersion = contextVersion,
			cellId = cellId,
			transferId = transferId,
			slotPath = captureSlotPath(root.Parent),
			mutationEpoch = stableEpoch,
			baseRevision = baseRevision,
		}, "art.capture.accepted", 15)
		captureBusy = false
		if proposal == nil then
			setStatus("Checkpoint proposal failed", proposalProblem or "daemon rejected the proposal", colors.danger)
			return
		end
		if proposal.status == "accepted" and type(proposal.revisionId) == "string" then
			root:SetAttribute("NeuManBaseRevision", proposal.revisionId)
			BridgeClient:recordAcceptedArtHead(proposal.revisionId, proposal.stateRoot)
		end
		if (ChangeTracker.epochs[cellId] or 0) == stableEpoch then
			ChangeTracker:accept(cellId)
			if proposal.status == "accepted" then
				setStatus(
					"Checkpoint accepted",
					string.format(
						"%s • queued for %d other Studio session(s)",
						tostring(proposal.revisionId),
						tonumber(proposal.fanoutSessions) or 0
					),
					colors.success
				)
			else
				setStatus(
					"Checkpoint proposed",
					"Revision " .. tostring(proposal.revisionId) .. " awaits channel acceptance.",
					colors.success
				)
			end
		else
			setStatus(
				"Checkpoint uploaded • newer local edits remain",
				"The proposal contains epoch " .. tostring(stableEpoch) .. "; checkpoint again when ready.",
				colors.warning
			)
		end
	end)
end

-- Safe staged apply ----------------------------------------------------------

local pendingApply: JsonMap? = nil
local activeApplyRecordingId: string? = nil

local function encodePathSegment(value: string): string
	local escaped = string.gsub(value, "~", "~0")
	escaped = string.gsub(escaped, "/", "~1")
	return escaped
end

local function decodePathSegment(value: string): string?
	local output = ""
	local index = 1
	while index <= #value do
		local character = string.sub(value, index, index)
		if character ~= "~" then
			output ..= character
			index += 1
		else
			local escape = string.sub(value, index, index + 1)
			if escape == "~0" then
				output ..= "~"
			elseif escape == "~1" then
				output ..= "/"
			else
				return nil
			end
			index += 2
		end
	end
	return output
end

local function dataModelPath(instance: Instance): string?
	local parts = {}
	local cursor: Instance? = instance
	while cursor ~= nil and cursor ~= game do
		table.insert(parts, 1, encodePathSegment(cursor.Name))
		cursor = cursor.Parent
	end
	if cursor ~= game or #parts == 0 then
		return nil
	end
	return "/" .. table.concat(parts, "/")
end

local function resolveParentPath(path: string): Instance?
	if #path == 0 or #path > 1024 or string.sub(path, 1, 1) ~= "/" then
		return nil
	end
	local parts = string.split(path, "/")
	if parts[1] == "" then
		table.remove(parts, 1)
	end
	if parts[1] == "game" then
		table.remove(parts, 1)
	end
	local firstEncoded = table.remove(parts, 1)
	local first = if firstEncoded == nil then nil else decodePathSegment(firstEncoded)
	if first == nil or first == "" then
		return nil
	end
	local ok, cursor = pcall(function()
		return game:GetService(first)
	end)
	if not ok then
		return nil
	end
	for _, encodedPart in parts do
		local part = decodePathSegment(encodedPart)
		if part == nil or part == "" then
			return nil
		end
		cursor = cursor:FindFirstChild(part)
		if cursor == nil then
			return nil
		end
	end
	return cursor
end

local function downloadCellBytes(cell: JsonMap): (string?, string?)
	if type(cell.data) == "string" then
		local decodeOk, bytes = pcall(base64UrlDecode, cell.data)
		return if decodeOk then bytes else nil, if decodeOk then nil else "Incoming inline base64 is invalid."
	end
	local download = cell.download
	if
		type(download) ~= "table"
		or type(download.url) ~= "string"
		or type(download.token) ~= "string"
		or #download.token < 32
		or #download.token > 256
		or not isNumericLoopbackUrl(download.url, "http")
	then
		return nil, "Incoming cell has no valid authenticated payload source."
	end
	local discovery = BridgeClient.discovery
	local webSocketUrl = if type(discovery) == "table" then discovery.webSocketUrl else nil
	if type(webSocketUrl) ~= "string" then
		return nil, "Bridge discovery context is unavailable for download validation."
	end
	local authority = string.match(webSocketUrl, "^ws://(127%.0%.0%.1:%d+)/v1/studio$")
	if
		authority == nil
		or string.sub(download.url, 1, #("http://" .. authority .. "/v1/studio/download/"))
			~= "http://" .. authority .. "/v1/studio/download/"
	then
		return nil, "Incoming download endpoint does not match the authenticated bridge."
	end
	setStatus("Downloading accepted native cell", tostring(cell.cellId), colors.warning)
	local ok, response = pcall(function()
		return HttpService:RequestAsync({
			Url = download.url,
			Method = "GET",
			Headers = {
				Authorization = "NeuManTransfer " .. download.token,
				["X-NeuMan-Session"] = BridgeClient.sessionId,
				Accept = "application/x-roblox-rbxm",
			},
		})
	end)
	if not ok or response.Success ~= true or type(response.Body) ~= "string" then
		return nil, "The authenticated native-cell download failed."
	end
	if #response.Body > MAX_CELL_BYTES then
		return nil, "The downloaded native cell exceeds the 96 MiB limit."
	end
	return response.Body, nil
end

local function deserializeCell(cell: JsonMap): (Instance?, string?)
	if not isCellId(cell.cellId) or type(cell.contentHash) ~= "string" or type(cell.sizeBytes) ~= "number" then
		return nil, "Incoming cell metadata is invalid."
	end
	if cell.sizeBytes > MAX_CELL_BYTES then
		return nil, "Incoming cell exceeds the bounded apply profile."
	end
	local bytes, payloadProblem = downloadCellBytes(cell)
	if bytes == nil then
		return nil, payloadProblem
	end
	if #bytes ~= cell.sizeBytes or blake3Label(bytes) ~= cell.contentHash then
		return nil, "Incoming cell size/hash verification failed."
	end
	local deserializeOk, roots = pcall(function()
		return (SerializationService :: any):DeserializeInstancesAsync(buffer.fromstring(bytes))
	end)
	if not deserializeOk or type(roots) ~= "table" or #roots ~= 1 or typeof(roots[1]) ~= "Instance" then
		return nil, "SerializationService could not stage exactly one cell root."
	end
	local root = roots[1] :: Instance
	if root.Parent ~= nil or root:GetAttribute(CELL_ATTRIBUTE) ~= cell.cellId then
		root:Destroy()
		return nil, "Staged root identity did not match the command."
	end
	for _, descendant in root:GetDescendants() do
		if isForbiddenContent(descendant) then
			root:Destroy()
			return nil, "Incoming cell contains forbidden executable/network content."
		end
		if descendant:GetAttribute(CELL_ATTRIBUTE) ~= nil then
			root:Destroy()
			return nil, "Incoming cell contains a nested managed cell boundary."
		end
	end
	return root, nil
end

local function sendApplyReceipt(command: JsonMap, status: string, verification: string, problem: string?)
	local contextVersion = BridgeClient:contextVersion()
	if contextVersion == nil then
		return
	end
	pcall(function()
		BridgeClient:sendEnvelope("art.apply.receipt", {
			contextVersion = contextVersion,
			commandId = command.commandId,
			revisionId = command.arguments.revisionId,
			status = status,
			verification = verification,
			problem = problem,
		})
	end)
end

local function applyPending()
	local command = pendingApply
	if command == nil then
		setStatus("Nothing to apply", "Wait for an accepted incoming revision.", colors.muted)
		return
	end
	if RunService:IsRunning() or not BridgeClient:isBound() then
		setStatus("Apply blocked", "Apply requires edit mode and an exact live context.", colors.danger)
		return
	end
	local contextVersion = BridgeClient:contextVersion()
	if command.contextVersion ~= contextVersion or command.command ~= "art.apply" then
		setStatus("Apply blocked", "The incoming command is stale or unsupported.", colors.danger)
		return
	end
	local arguments = command.arguments
	if type(arguments) ~= "table" or type(arguments.cells) ~= "table" or #arguments.cells == 0 then
		setStatus("Apply blocked", "This revision has no bounded native cell payloads.", colors.danger)
		return
	end
	local existingCells, registryProblems = scanCells()
	local preflightEpochs: JsonMap = {}
	if #registryProblems > 0 then
		setStatus("Apply blocked", table.concat(registryProblems, "; "), colors.danger)
		return
	end
	for _, cell in arguments.cells do
		if ChangeTracker:isDirty(cell.cellId) then
			setStatus(
				"Conflict • local cell is dirty",
				"Keep local, checkpoint it, or explicitly discard before applying " .. cell.cellId,
				colors.danger
			)
			return
		end
		local targetParent = resolveParentPath(cell.parentPath)
		if targetParent == nil then
			setStatus(
				"Apply blocked",
				"Configured parent path does not exist: " .. tostring(cell.parentPath),
				colors.danger
			)
			return
		end
		local parentCursor: Instance? = targetParent
		while parentCursor ~= nil and parentCursor ~= game do
			if parentCursor:GetAttribute(CELL_ATTRIBUTE) ~= nil then
				setStatus("Apply blocked", "The destination crosses another managed cell boundary.", colors.danger)
				return
			end
			parentCursor = parentCursor.Parent
		end
		preflightEpochs[cell.cellId] = ChangeTracker.epochs[cell.cellId] or 0
	end

	setStatus("Staging incoming revision", tostring(arguments.revisionId), colors.warning)
	task.spawn(function()
		local staged: JsonMap = {}
		for _, cell in arguments.cells do
			local root, problem = deserializeCell(cell)
			if root == nil then
				for _, stagedItem in staged do
					stagedItem.root:Destroy()
				end
				setStatus("Apply preflight failed", problem or "could not stage cell", colors.danger)
				sendApplyReceipt(command, "failed", "failed", problem)
				return
			end
			table.insert(staged, {
				root = root,
				cell = cell,
				parent = resolveParentPath(cell.parentPath),
				old = existingCells[cell.cellId],
				oldParent = if existingCells[cell.cellId] then existingCells[cell.cellId].Parent else nil,
			})
		end
		for _, item in staged do
			if
				ChangeTracker:isDirty(item.cell.cellId)
				or (ChangeTracker.epochs[item.cell.cellId] or 0) ~= preflightEpochs[item.cell.cellId]
			then
				for _, stagedItem in staged do
					stagedItem.root:Destroy()
				end
				setStatus(
					"Conflict • cell changed during preflight",
					"No Studio mutation was made. Resolve local edits before applying " .. item.cell.cellId,
					colors.danger
				)
				sendApplyReceipt(command, "failed", "failed", "local cell changed during preflight")
				return
			end
		end

		local recordingId: string? = nil
		local beginOk, beginResult = pcall(function()
			return ChangeHistoryAny:TryBeginRecording("NeuManApply", "Apply NeuMan art revision")
		end)
		if beginOk then
			recordingId = beginResult
		end
		if type(recordingId) ~= "string" then
			for _, item in staged do
				item.root:Destroy()
			end
			setStatus(
				"Apply blocked",
				"ChangeHistoryService recording is unavailable; no changes were made.",
				colors.danger
			)
			sendApplyReceipt(command, "failed", "failed", "change recording unavailable")
			return
		end
		activeApplyRecordingId = recordingId

		ChangeTracker.suppressed = true
		local commitOk, commitProblem = pcall(function()
			for _, item in staged do
				if item.old ~= nil then
					item.old.Parent = nil
				end
			end
			for _, item in staged do
				item.root.Parent = item.parent
			end
			local verified, duplicateProblems = scanCells()
			if #duplicateProblems > 0 then
				error(table.concat(duplicateProblems, "; "))
			end
			for _, item in staged do
				if verified[item.cell.cellId] ~= item.root then
					error("cell verification failed for " .. item.cell.cellId)
				end
			end
			for _, item in staged do
				if item.old ~= nil then
					item.old:Destroy()
				end
			end
		end)
		if not commitOk then
			local restored = pcall(function()
				for _, item in staged do
					item.root.Parent = nil
					item.root:Destroy()
					if item.old ~= nil then
						item.old.Parent = item.oldParent
					end
				end
			end)
			pcall(function()
				ChangeHistoryAny:FinishRecording(recordingId, Enum.FinishRecordingOperation.Cancel)
			end)
			activeApplyRecordingId = nil
			ChangeTracker.suppressed = false
			local status = if restored then "failed" else "recovery-required"
			setStatus(
				if restored then "Apply rolled back" else "Recovery required",
				tostring(commitProblem),
				colors.danger
			)
			sendApplyReceipt(command, status, "failed", tostring(commitProblem))
			return
		end
		local finishOk = pcall(function()
			ChangeHistoryAny:FinishRecording(recordingId, Enum.FinishRecordingOperation.Commit)
		end)
		activeApplyRecordingId = nil
		if not finishOk then
			ChangeTracker.suppressed = false
			setStatus(
				"Recovery required",
				"Content changed but Studio could not commit the undo recording.",
				colors.danger
			)
			sendApplyReceipt(command, "recovery-required", "applied-unverified", "undo commit failed")
			return
		end
		for _, item in staged do
			ChangeTracker:unwatch(item.cell.cellId)
			item.root:SetAttribute("NeuManBaseRevision", arguments.revisionId)
			ChangeTracker:watch(item.root)
		end
		ChangeTracker.suppressed = false
		for _, item in staged do
			ChangeTracker:accept(item.cell.cellId)
		end
		pendingApply = nil
		incomingLabel.Text = "Incoming: none"
		setStatus(
			"Revision applied",
			tostring(arguments.revisionId) .. " • undoable as one Studio action",
			colors.success
		)
		sendApplyReceipt(command, "succeeded", "atomic-apply-completed", nil)
	end)
end

BridgeClient.onIncoming.Event:Connect(function(revision: JsonMap)
	incomingLabel.Text =
		string.format("Incoming: %s • %d cell(s)", tostring(revision.revisionId), #(revision.cellIds or {}))
	incomingLabel.TextColor3 = colors.warning
	setStatus("Accepted art available", tostring(revision.summary or revision.revisionId), colors.warning)
end)

BridgeClient.onCommand.Event:Connect(function(command: JsonMap)
	if command.command ~= "art.apply" or command.contextVersion ~= BridgeClient:contextVersion() then
		setStatus("Incoming command rejected", "Unsupported or stale mutation command.", colors.danger)
		return
	end
	if pendingApply ~= nil and pendingApply.idempotencyKey ~= command.idempotencyKey then
		setStatus("Incoming command rejected", "Resolve the existing pending revision before retrying.", colors.warning)
		sendApplyReceipt(command, "cancelled", "not-queued", "another apply is pending")
		return
	end
	pendingApply = command
	local arguments = command.arguments or {}
	incomingLabel.Text =
		string.format("Ready to preview: %s • %d cell(s)", tostring(arguments.revisionId), #(arguments.cells or {}))
	incomingLabel.TextColor3 = colors.warning
	BridgeClient:sendEnvelope("command.accepted", {
		contextVersion = BridgeClient:contextVersion(),
		commandId = command.commandId,
		revisionId = arguments.revisionId,
		status = "awaiting-user-preview",
	})
end)

-- UI and lifecycle wiring ----------------------------------------------------

local function refreshSelection()
	local root = selectedCell()
	if root ~= nil then
		local id = root:GetAttribute(CELL_ATTRIBUTE) :: string
		selectionLabel.Text =
			string.format("Selection: %s%s", id, if ChangeTracker:isDirty(id) then " • modified" else " • clean")
		selectionLabel.TextColor3 = if ChangeTracker:isDirty(id) then colors.warning else colors.success
	else
		local selection = Selection:Get()
		selectionLabel.Text = if #selection == 1
			then "Selection: unmanaged • " .. selection[1]:GetFullName()
			else "Selection: choose exactly one cell or eligible root"
		selectionLabel.TextColor3 = colors.muted
	end
end

local function registerSelection()
	if RunService:IsRunning() then
		setStatus("Registration blocked", "Stop play/test mode first.", colors.danger)
		return
	end
	if not BridgeClient:isBound() then
		setStatus("Registration blocked", "An exact daemon binding is required.", colors.danger)
		return
	end
	local selected = Selection:Get()
	if #selected ~= 1 then
		setStatus("Registration blocked", "Select exactly one eligible Model or Folder.", colors.danger)
		return
	end
	local root = selected[1]
	if root == Workspace.Terrain or root:IsA("LuaSourceContainer") or root:GetAttribute(CELL_ATTRIBUTE) ~= nil then
		setStatus("Registration blocked", "Terrain, scripts, and existing cells are not eligible.", colors.danger)
		return
	end
	local cursor = root.Parent
	while cursor ~= nil and cursor ~= game do
		if cursor:GetAttribute(CELL_ATTRIBUTE) ~= nil then
			setStatus("Registration blocked", "Nested art cells are not supported.", colors.danger)
			return
		end
		cursor = cursor.Parent
	end
	local id = "cell_" .. string.lower(HttpService:GenerateGUID(false))
	local cellPath = dataModelPath(root)
	if cellPath == nil then
		setStatus("Registration blocked", "The selection is not attached beneath the DataModel.", colors.danger)
		return
	end
	setStatus("Requesting registration intent", root:GetFullName(), colors.warning)
	local approved, approvalProblem = BridgeClient:request("command.request", {
		contextVersion = BridgeClient:contextVersion(),
		commandId = "op_" .. string.lower(HttpService:GenerateGUID(false)),
		command = "art.register-cell",
		arguments = {
			cellId = id,
			path = cellPath,
			className = root.ClassName,
			projectId = projectBox.Text,
		},
	}, "command.succeeded", 10)
	if approved == nil or approved.approved ~= true or approved.cellId ~= id then
		setStatus(
			"Registration denied",
			approvalProblem or "The daemon did not approve this ownership slot.",
			colors.danger
		)
		return
	end
	local recordingId = ChangeHistoryAny:TryBeginRecording("NeuManRegisterCell", "Register NeuMan art cell")
	if type(recordingId) ~= "string" then
		setStatus("Registration blocked", "Studio undo recording is unavailable.", colors.danger)
		return
	end
	root:SetAttribute(CELL_ATTRIBUTE, id)
	root:SetAttribute(PROJECT_ATTRIBUTE, projectBox.Text)
	ChangeHistoryAny:FinishRecording(recordingId, Enum.FinishRecordingOperation.Commit)
	ChangeTracker:watch(root)
	ChangeTracker:mark(root, "registered")
	BridgeClient:sendEnvelope("studio.selection-changed", {
		contextVersion = BridgeClient:contextVersion(),
		registeredCellId = id,
	})
	setStatus("Art cell registered", id .. " • checkpoint it to create the first proposal", colors.success)
	refreshSelection()
end

openButton.Click:Connect(function()
	widget.Enabled = not widget.Enabled
end)
checkpointButton.Click:Connect(checkpointSelected)
checkpointWidgetButton.MouseButton1Click:Connect(checkpointSelected)
applyButton.Click:Connect(applyPending)
applyWidgetButton.MouseButton1Click:Connect(applyPending)
registerButton.MouseButton1Click:Connect(registerSelection)
Selection.SelectionChanged:Connect(refreshSelection)
ChangeTracker.onChanged.Event:Connect(function(cellId: string, dirty: boolean, reason: string, epoch: number)
	refreshSelection()
	if BridgeClient:isBound() then
		pcall(function()
			BridgeClient:sendEnvelope("art.cell-dirty-changed", {
				contextVersion = BridgeClient:contextVersion(),
				cellId = cellId,
				dirty = dirty,
				reason = reason,
				mutationEpoch = epoch,
			})
		end)
	end
end)

saveIdentityButton.MouseButton1Click:Connect(function()
	if not string.match(projectBox.Text, "^[%w_%-]+$") or #projectBox.Text > 128 then
		setStatus("Invalid project ID", "Use the project ID from neuman.project.yaml.", colors.danger)
		return
	end
	if not string.match(channelBox.Text, "^[%w_/%-]+$") or #channelBox.Text > 128 then
		setStatus("Invalid channel", "Use a bounded channel name such as art/main.", colors.danger)
		return
	end
	settings:set("ProjectId", projectBox.Text)
	settings:set("Channel", channelBox.Text)
	setStatus("Identity saved", "Reconnect so the daemon can validate the new context.", colors.warning)
	BridgeClient:disconnect("Project identity changed; reconnect required.")
end)

connectButton.MouseButton1Click:Connect(function()
	setStatus("Discovering NeuMan", DISCOVERY_URL, colors.warning)
	task.spawn(function()
		local ok, problem = BridgeClient:connect()
		if not ok then
			setStatus("Connection needs attention", problem or "connection failed", colors.warning)
		end
	end)
end)

pairButton.MouseButton1Click:Connect(function()
	setStatus("Pairing", "Confirm this Studio installation in the desktop.", colors.warning)
	task.spawn(function()
		local ok, problem = BridgeClient:pair(pairingBox.Text)
		if ok then
			setStatus("Paired", "The credential is local-bridge-only. Connecting…", colors.success)
			local connected, connectProblem = BridgeClient:connect()
			if not connected then
				setStatus("Paired • connection failed", connectProblem or "retry Connect", colors.warning)
			end
		else
			setStatus("Pairing needs attention", problem or "pairing failed", colors.warning)
		end
	end)
end)

pcall(function()
	RunService:GetPropertyChangedSignal("RunState"):Connect(function()
		if BridgeClient.connected then
			BridgeClient:sendEnvelope("studio.run-state-changed", {
				contextVersion = BridgeClient:contextVersion(),
				runState = if RunService:IsRunning() then "play" else "edit",
			})
		end
	end)
end)

plugin.Unloading:Connect(function()
	if activeApplyRecordingId ~= nil then
		pcall(function()
			ChangeHistoryAny:FinishRecording(activeApplyRecordingId :: string, Enum.FinishRecordingOperation.Cancel)
		end)
		activeApplyRecordingId = nil
		ChangeTracker.suppressed = false
	end
	if BridgeClient.connected then
		pcall(function()
			BridgeClient:sendEnvelope("session.goodbye", { reason = "plugin-unloading" })
		end)
	end
	BridgeClient:disconnect("plugin unloading")
	for _, connections in ChangeTracker.connections do
		for _, connection in connections do
			connection:Disconnect()
		end
	end
end)

refreshSelection()
if #compatibilityProblems > 0 then
	setStatus("Read-only incompatible", table.concat(compatibilityProblems, "; "), colors.danger)
else
	setStatus(
		"Ready to connect",
		"NeuMan Studio Plugin " .. PLUGIN_VERSION .. " • protocol " .. PROTOCOL_VERSION,
		colors.muted
	)
end
