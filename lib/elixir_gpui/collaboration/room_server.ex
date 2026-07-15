defmodule ElixirGpui.Collaboration.RoomServer do
  @moduledoc false

  use Yex.DocServer, restart: :transient

  alias Yex.Sync

  @readme """
  # Elixir GPUI Workspace

  Welcome to the collaborative Markdown editor built with GPUI, Phoenix, and Yrs.

  ## Getting started

  - Create a document with the **+** button in the sidebar.
  - Open the same workspace in another browser to edit together.
  - Switch between Editor, Split, and Preview modes from the toolbar.

  Changes and collaborator cursors are synchronized in real time.
  """

  @registry ElixirGpui.Collaboration.Registry
  @supervisor ElixirGpui.Collaboration.Supervisor

  def start_room_link(document_id) do
    start_link([document_id: document_id], name: via(document_id))
  end

  def child_spec(document_id) do
    %{
      id: {__MODULE__, document_id},
      start: {__MODULE__, :start_room_link, [document_id]},
      restart: :transient
    }
  end

  def ensure_started(document_id) do
    case Registry.lookup(@registry, document_id) do
      [{pid, _value}] ->
        {:ok, pid}

      [] ->
        case DynamicSupervisor.start_child(@supervisor, {__MODULE__, document_id}) do
          {:ok, pid} -> {:ok, pid}
          {:error, {:already_started, pid}} -> {:ok, pid}
          {:error, reason} -> {:error, reason}
        end
    end
  end

  @impl true
  def init(options, state) do
    document_id = Keyword.fetch!(options, :document_id)
    seed_document(document_id, state.doc)
    {:ok, assign(state, document_id: document_id, topic: "documents:#{document_id}")}
  end

  @impl true
  def handle_update_v1(_doc, update, origin, state) do
    with {:ok, sync_update} <- Sync.get_update(update),
         {:ok, message} <- Sync.message_encode({:sync, sync_update}) do
      payload = %{message: Base.encode64(message)}

      if is_pid(origin) do
        ElixirGpuiWeb.Endpoint.broadcast_from(origin, state.assigns.topic, "yjs", payload)
      else
        ElixirGpuiWeb.Endpoint.broadcast(state.assigns.topic, "yjs", payload)
      end
    end

    {:noreply, state}
  end

  defp via(document_id), do: {:via, Registry, {@registry, document_id}}

  defp seed_document("readme", doc) do
    doc
    |> Yex.Doc.get_text("content")
    |> Yex.Text.insert(0, @readme)
  end

  defp seed_document(_document_id, _doc), do: :ok
end
