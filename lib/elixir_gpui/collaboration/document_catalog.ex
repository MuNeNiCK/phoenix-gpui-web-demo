defmodule ElixirGpui.Collaboration.DocumentCatalog do
  @moduledoc false

  use Agent

  @topic "workspace:documents"
  @document_id ~r/\A[a-zA-Z0-9_-]{1,64}\z/
  @default_documents [%{"id" => "readme", "title" => "README.md"}]

  def start_link(_options) do
    Agent.start_link(fn -> index(@default_documents) end, name: __MODULE__)
  end

  def list do
    Agent.get(__MODULE__, fn documents ->
      documents
      |> Map.values()
      |> Enum.sort_by(&String.downcase(&1["title"]))
    end)
  end

  def merge(documents) when is_list(documents) do
    valid_documents = Enum.filter(documents, &valid?/1)

    changed? =
      Agent.get_and_update(__MODULE__, fn current ->
        updated = Map.merge(current, index(valid_documents))
        {updated != current, updated}
      end)

    if changed? do
      Phoenix.PubSub.broadcast(ElixirGpui.PubSub, @topic, {:documents_updated, list()})
    end

    :ok
  end

  def merge(_documents), do: :ok

  def subscribe do
    Phoenix.PubSub.subscribe(ElixirGpui.PubSub, @topic)
  end

  defp valid?(%{"id" => id, "title" => title}) do
    is_binary(id) and Regex.match?(@document_id, id) and is_binary(title) and
      String.trim(title) != "" and byte_size(title) <= 256
  end

  defp valid?(_document), do: false

  defp index(documents), do: Map.new(documents, &{&1["id"], &1})
end
