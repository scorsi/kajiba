// kajiba-cli — a thin client over kajiba's gRPC API (0007). Empty shell until J4.
package main

import (
	"fmt"
	"os"

	"github.com/scorsi/kajiba/cli/internal/version"
	"github.com/spf13/cobra"
)

func main() {
	root := &cobra.Command{
		Use:     "kajiba",
		Short:   "kajiba-cli — a thin client for the kajiba forge",
		Version: version.Version,
	}

	if err := root.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
