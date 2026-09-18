package store_test

import (
	"testing"

	"github.com/madelog/server/internal/store"
	"github.com/madelog/server/internal/store/storetest"
)

func TestMemoryStoreConformance(t *testing.T) {
	storetest.Run(t, func(t *testing.T) store.Store {
		return store.NewMemory()
	})
}
