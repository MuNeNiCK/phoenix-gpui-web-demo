defmodule ElixirGpuiWeb.ChannelCase do
  use ExUnit.CaseTemplate

  using do
    quote do
      import Phoenix.ChannelTest

      @endpoint ElixirGpuiWeb.Endpoint
    end
  end
end
